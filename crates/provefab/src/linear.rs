//! Linear as an issue tracker (issue trackers spec §7): the GraphQL API with a
//! personal API key. Labels and comments only: Provefab never changes an
//! issue's state.

use std::collections::HashSet;

use serde_json::{Value, json};
use tokio::sync::OnceCell;

use crate::forge::{Comment, ForgeError, ISSUE_LIMIT, Issue, issue_limit_warning, with_prefix};
use crate::ports::Tracker;
use crate::tracker::{http_client, http_error, number_of, send, utc_seconds};

pub const API: &str = "https://api.linear.app/graphql";
const SERVICE: &str = "linear";

/// The issue fields Provefab reads, shared by the queries below.
macro_rules! issue_fields {
    () => {
        "id identifier number title description url creator { id } labels { nodes { id name } } state { type }"
    };
}

const OPEN: &str = concat!(
    "query($team: String!, $label: String!, $after: String) { issues(first: 100, after: $after, filter: { team: { key: { eq: $team } }, labels: { name: { eq: $label } }, state: { type: { nin: [\"completed\", \"canceled\", \"duplicate\"] } } }) { nodes { ",
    issue_fields!(),
    " } pageInfo { hasNextPage endCursor } } }"
);
const ISSUE: &str = concat!(
    "query($id: String!) { issue(id: $id) { ",
    issue_fields!(),
    " } }"
);
const COMMENTS: &str = "query($id: String!, $after: String) { issue(id: $id) { comments(first: 100, after: $after) { nodes { body createdAt user { id app } } pageInfo { hasNextPage endCursor } } } }";
const COMMENT: &str = "mutation($id: String!, $body: String!) { commentCreate(input: { issueId: $id, body: $body }) { success } }";
/// Added and removed ids, never the whole set (`labelIds`): a label a person
/// adds between our read and this update survives.
const UPDATE_LABELS: &str = "mutation($id: String!, $added: [String!]!, $removed: [String!]!) { issueUpdate(id: $id, input: { addedLabelIds: $added, removedLabelIds: $removed }) { success } }";
const FIND_LABEL: &str = "query($name: String!) { issueLabels(filter: { name: { eq: $name } }) { nodes { id name team { id } } } }";
const CREATE_LABEL: &str = "mutation($input: IssueLabelCreateInput!) { issueLabelCreate(input: $input) { success issueLabel { id } } }";
const TEAM: &str = "query($key: String!) { teams(filter: { key: { eq: $key } }) { nodes { id } } }";
const CHECK: &str = "query($key: String!) { viewer { id organization { urlKey } } teams(filter: { key: { eq: $key } }) { nodes { id } } }";

/// Most pages one listing reads: enough for `ISSUE_LIMIT` issues, and a
/// guard against a server that always says there is more.
const ISSUE_PAGES: usize = ISSUE_LIMIT / 100 + 1;
/// Most comment pages read for one issue: 5,000 comments.
const COMMENT_PAGES: usize = 50;

pub struct Linear {
    /// The GraphQL endpoint; a test server's address in tests.
    pub api: String,
    /// The team key: `ENG` in `ENG-123`.
    pub team: String,
    key: String,
    http: reqwest::Client,
    team_id: OnceCell<String>,
}

fn not_found(message: String) -> ForgeError {
    ForgeError::Tracker {
        service: SERVICE,
        status: Some(404),
        message,
    }
}

/// The status a GraphQL `errors` answer stands for: Linear reports most
/// failures in the body, whatever the HTTP status (spec §8). `None` without errors.
fn graphql_status(v: &Value) -> Option<u16> {
    let first = v["errors"].as_array()?.first()?;
    let code = ["/extensions/code", "/extensions/type"]
        .iter()
        .filter_map(|p| first.pointer(p).and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(" ")
        .to_uppercase();
    let message = first["message"].as_str().unwrap_or_default().to_lowercase();
    Some(if code.contains("RATELIMIT") {
        429
    } else if code.contains("AUTHENTICATION") || message.contains("authentication") {
        401
    } else if code.contains("FORBIDDEN")
        || message.contains("forbidden")
        || message.contains("permission")
    {
        403
    } else if message.contains("not found") {
        404
    } else if code.contains("INTERNAL") {
        500
    } else {
        400
    })
}

fn succeeded(d: &Value, what: &str) -> Result<(), ForgeError> {
    if d[what]["success"].as_bool() == Some(true) {
        Ok(())
    } else {
        Err(ForgeError::Parse(
            format!("linear {what}"),
            "not successful".into(),
        ))
    }
}

/// A person's comment; one without a user is an integration's and one by an
/// app user (`app: true`) a bot's: both skipped (plan decision 4).
fn comment_of(c: &Value) -> Option<Comment> {
    if c.pointer("/user/app").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let author = c.pointer("/user/id")?.as_str()?.to_string();
    let created = c["createdAt"].as_str()?;
    let Some(created_at) = utc_seconds(created) else {
        eprintln!("provefab: a linear comment with an unreadable time {created:?} is skipped");
        return None;
    };
    Some(Comment {
        author,
        // Guests also map to MEMBER until the real run confirms Linear's guest
        // field; re-open then (map guests to NONE).
        association: "MEMBER".into(),
        body: c["body"].as_str().unwrap_or_default().to_string(),
        created_at,
    })
}

/// The cursor of the page after `page`, unless the listing should stop: no
/// next page, `pages` already read up to `cap`, or a cursor seen before (a
/// server that always says there is more cannot keep Provefab looping). The
/// log line names which of the last two stopped it.
fn next_page(
    page: &Value,
    seen: &mut HashSet<String>,
    pages: usize,
    cap: usize,
    what: &str,
) -> Option<String> {
    let cursor = page_end(page)?;
    if let Some(reason) = stop_reason(&cursor, seen, pages, cap) {
        eprintln!("provefab: {}", stop_message(what, pages, reason));
        return None;
    }
    Some(cursor)
}

/// Why listing stops at `cursor`, if it does. The cap is checked first, so a
/// cursor is not recorded once the cap is reached.
fn stop_reason(
    cursor: &str,
    seen: &mut HashSet<String>,
    pages: usize,
    cap: usize,
) -> Option<&'static str> {
    if pages >= cap {
        Some("page cap")
    } else if !seen.insert(cursor.to_string()) {
        Some("repeated cursor")
    } else {
        None
    }
}

fn stop_message(what: &str, pages: usize, reason: &str) -> String {
    format!("linear {what}: stopped after {pages} pages ({reason})")
}

fn page_end(page: &Value) -> Option<String> {
    let more = page
        .pointer("/pageInfo/hasNextPage")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let cursor = page.pointer("/pageInfo/endCursor").and_then(Value::as_str);
    cursor.filter(|_| more).map(str::to_string)
}

impl Linear {
    pub fn new(api: &str, team: &str, key: String) -> Self {
        Self {
            api: api.to_string(),
            team: team.to_string(),
            key,
            http: http_client(),
            team_id: OnceCell::new(),
        }
    }

    fn ident(&self, number: u64) -> String {
        format!("{}-{number}", self.team)
    }

    /// One GraphQL request: its `data`, or the error it stands for.
    async fn query(&self, query: &str, variables: Value) -> Result<Value, ForgeError> {
        let secrets = [self.key.as_str()];
        let body = json!({"query": query, "variables": variables});
        // Linear wants the personal key as is, without `Bearer` (spec §7).
        let (status, retry_after, text) = send(SERVICE, &secrets, || {
            self.http
                .post(&self.api)
                .header("Authorization", &self.key)
                .json(&body)
        })
        .await?;
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        // A status that says "try later" stands whatever the body says, so an
        // unknown error code cannot turn it permanent; otherwise the body
        // decides (a 400 carrying RATELIMITED is a rate limit), then the status.
        let failed = if status == 408 || status == 429 || status >= 500 {
            Some(status)
        } else {
            graphql_status(&v).or((!(200..300).contains(&status)).then_some(status))
        };
        if let Some(code) = failed {
            return Err(http_error(SERVICE, code, retry_after, &text, &secrets));
        }
        match v.get("data") {
            Some(d) if !d.is_null() => Ok(d.clone()),
            _ => Err(ForgeError::Parse(
                "linear".into(),
                "no data in the answer".into(),
            )),
        }
    }

    /// The issue's uuid, which mutations need: a local error when the answer
    /// has none, so no mutation goes out with an empty or null id.
    async fn issue_with_id(&self, number: u64) -> Result<(String, Value), ForgeError> {
        let issue = self.issue_value(number).await?;
        let id = issue["id"].as_str().map(str::to_string).ok_or_else(|| {
            ForgeError::Parse(
                "linear issue".into(),
                format!("no id for {}", self.ident(number)),
            )
        })?;
        Ok((id, issue))
    }

    async fn issue_value(&self, number: u64) -> Result<Value, ForgeError> {
        let ident = self.ident(number);
        let d = self.query(ISSUE, json!({"id": ident})).await?;
        match d.get("issue") {
            Some(i) if !i.is_null() => Ok(i.clone()),
            _ => Err(not_found(format!("{ident} not found"))),
        }
    }

    fn issue_of(&self, v: &Value) -> Option<Issue> {
        let ident = v["identifier"].as_str()?;
        Some(Issue {
            number: number_of(ident, &self.team)?,
            key: Some(ident.to_string()),
            title: v["title"].as_str()?.to_string(),
            body: v["description"].as_str().unwrap_or_default().to_string(),
            url: v["url"].as_str()?.to_string(),
            author: v["creator"]["id"].as_str().unwrap_or_default().to_string(),
            labels: v
                .pointer("/labels/nodes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|l| l["name"].as_str().map(str::to_string))
                .collect(),
        })
    }

    async fn team_id(&self) -> Result<&str, ForgeError> {
        self.team_id
            .get_or_try_init(|| async {
                let d = self.query(TEAM, json!({"key": self.team})).await?;
                d.pointer("/teams/nodes/0/id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| not_found(format!("team {} not found", self.team)))
            })
            .await
            .map(String::as_str)
    }

    /// A label of that name usable on this team's issues: the team's own or a
    /// workspace label.
    async fn find_label(&self, name: &str) -> Result<Option<String>, ForgeError> {
        let team = self.team_id().await?.to_string();
        let d = self.query(FIND_LABEL, json!({"name": name})).await?;
        Ok(d.pointer("/issueLabels/nodes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|l| {
                l["name"].as_str() == Some(name)
                    && (l["team"].is_null()
                        || l.pointer("/team/id").and_then(Value::as_str) == Some(team.as_str()))
            })
            .and_then(|l| l["id"].as_str().map(str::to_string)))
    }

    async fn create_label(
        &self,
        name: &str,
        color: &str,
        description: &str,
    ) -> Result<String, ForgeError> {
        let team = self.team_id().await?.to_string();
        let input = json!({"name": name, "color": format!("#{color}"), "description": description, "teamId": team});
        let d = self.query(CREATE_LABEL, json!({"input": input})).await?;
        d.pointer("/issueLabelCreate/issueLabel/id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                ForgeError::Parse("linear issueLabelCreate".into(), "no label id".into())
            })
    }

    /// For `provefab doctor`: the key authenticates and the team is readable.
    pub async fn check(&self) -> Result<String, ForgeError> {
        let d = self.query(CHECK, json!({"key": self.team})).await?;
        let workspace = d
            .pointer("/viewer/organization/urlKey")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        if d.pointer("/teams/nodes/0/id").is_none() {
            return Err(not_found(format!(
                "team {} not found in workspace {workspace}",
                self.team
            )));
        }
        Ok(format!(
            "workspace {workspace}; credentials accepted; team {} readable",
            self.team
        ))
    }
}

impl Tracker for Linear {
    async fn open_issues(&self, slug: &str, label: &str) -> Result<Vec<Issue>, ForgeError> {
        let mut out = Vec::new();
        let mut after: Option<String> = None;
        let (mut seen, mut pages) = (HashSet::new(), 0);
        loop {
            pages += 1;
            let d = self
                .query(
                    OPEN,
                    json!({"team": self.team, "label": label, "after": after}),
                )
                .await?;
            let page = &d["issues"];
            out.extend(
                page["nodes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|i| self.issue_of(i)),
            );
            if out.len() >= ISSUE_LIMIT {
                break;
            }
            after = next_page(page, &mut seen, pages, ISSUE_PAGES, "issues");
            if after.is_none() {
                break;
            }
        }
        out.truncate(ISSUE_LIMIT);
        if let Some(warning) = issue_limit_warning(slug, label, out.len()) {
            eprintln!("{warning}");
        }
        Ok(out)
    }

    async fn issue(&self, _slug: &str, number: u64) -> Result<Issue, ForgeError> {
        let v = self.issue_value(number).await?;
        self.issue_of(&v).ok_or_else(|| {
            ForgeError::Parse(
                "linear issue".into(),
                format!("no title or URL for {}", self.ident(number)),
            )
        })
    }

    async fn comments(&self, _slug: &str, number: u64) -> Result<Vec<Comment>, ForgeError> {
        let ident = self.ident(number);
        let mut out = Vec::new();
        let mut after: Option<String> = None;
        let (mut seen, mut pages) = (HashSet::new(), 0);
        loop {
            pages += 1;
            let d = self
                .query(COMMENTS, json!({"id": ident, "after": after}))
                .await?;
            if d["issue"].is_null() {
                return Err(not_found(format!("{ident} not found")));
            }
            let page = &d["issue"]["comments"];
            out.extend(
                page["nodes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(comment_of),
            );
            after = next_page(page, &mut seen, pages, COMMENT_PAGES, "comments");
            if after.is_none() {
                break;
            }
        }
        Ok(out)
    }

    async fn comment(&self, _slug: &str, number: u64, body: &str) -> Result<(), ForgeError> {
        let (id, _) = self.issue_with_id(number).await?;
        let d = self
            .query(COMMENT, json!({"id": id, "body": with_prefix(body)}))
            .await?;
        succeeded(&d, "commentCreate")
    }

    async fn edit_labels(
        &self,
        _slug: &str,
        number: u64,
        add: &[&str],
        remove: &[&str],
    ) -> Result<(), ForgeError> {
        if add.is_empty() && remove.is_empty() {
            return Ok(());
        }
        let (id, issue) = self.issue_with_id(number).await?;
        let current: Vec<(String, String)> = issue
            .pointer("/labels/nodes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|l| {
                Some((
                    l["id"].as_str()?.to_string(),
                    l["name"].as_str()?.to_string(),
                ))
            })
            .collect();
        let removed: Vec<String> = current
            .iter()
            .filter(|(_, name)| remove.contains(&name.as_str()))
            .map(|(id, _)| id.clone())
            .collect();
        let mut added: Vec<String> = Vec::new();
        for name in add {
            if current.iter().any(|(_, n)| n == name) {
                continue;
            }
            let id = match self.find_label(name).await? {
                Some(id) => id,
                None => self.create_label(name, "ededed", "Provefab").await?,
            };
            if !added.contains(&id) {
                added.push(id);
            }
        }
        if added.is_empty() && removed.is_empty() {
            return Ok(());
        }
        let d = self
            .query(
                UPDATE_LABELS,
                json!({"id": id, "added": added, "removed": removed}),
            )
            .await?;
        succeeded(&d, "issueUpdate")
    }

    /// Creates a team label with this colour when none of that name exists (spec §7).
    async fn ensure_label(
        &self,
        _slug: &str,
        name: &str,
        color: &str,
        description: &str,
    ) -> Result<(), ForgeError> {
        if self.find_label(name).await?.is_none() {
            self.create_label(name, color, description).await?;
        }
        Ok(())
    }

    async fn issue_open(&self, _slug: &str, number: u64) -> Result<bool, ForgeError> {
        let v = self.issue_value(number).await?;
        match v.pointer("/state/type").and_then(Value::as_str) {
            // Linear's closed state types; `duplicate` is listed beside them in
            // its schema (WorkflowState.type), so it counts as closed too.
            Some(state) => Ok(!matches!(state, "completed" | "canceled" | "duplicate")),
            None => Err(ForgeError::Parse("linear issue".into(), "no state".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, body_string_contains, header, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const KEY: &str = "lin_api_SECRET_123";

    #[test]
    fn stop_message_names_the_reason() {
        assert_eq!(
            stop_message("issues", 5, "page cap"),
            "linear issues: stopped after 5 pages (page cap)"
        );
        assert_eq!(
            stop_message("issues", 2, "repeated cursor"),
            "linear issues: stopped after 2 pages (repeated cursor)"
        );
    }

    #[test]
    fn stop_reason_tells_cap_from_repeated_cursor() {
        let mut seen = HashSet::new();
        assert_eq!(stop_reason("c1", &mut seen, 1, 3), None);
        assert_eq!(stop_reason("c1", &mut seen, 2, 3), Some("repeated cursor"));
        assert_eq!(stop_reason("c2", &mut seen, 3, 3), Some("page cap"));
    }

    #[test]
    fn docs_name_both_stop_reasons() {
        let guide = include_str!("../../../docs/guide/trackers.md");
        let readme = include_str!("../../../README.md");
        for reason in ["page cap", "repeated cursor"] {
            assert!(guide.contains(&format!("({reason})")), "guide: {reason}");
            assert!(readme.contains(reason), "readme: {reason}");
        }
    }

    fn linear(server: &MockServer) -> Linear {
        Linear::new(&server.uri(), "ENG", KEY.into())
    }

    fn node(n: u64, state: &str) -> Value {
        json!({
            "id": format!("uuid-{n}"), "identifier": format!("ENG-{n}"), "number": n,
            "title": format!("Ticket {n}"), "description": "Crash on **start**",
            "url": format!("https://linear.app/acme/issue/ENG-{n}/ticket-{n}"),
            "creator": {"id": "user-alice"},
            "labels": {"nodes": [{"id": "l-trigger", "name": "provefab"}]},
            "state": {"type": state}
        })
    }

    fn data(v: Value) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({"data": v}))
    }

    async fn on(server: &MockServer, needle: &str, answer: ResponseTemplate) {
        Mock::given(method("POST"))
            .and(body_string_contains(needle))
            .respond_with(answer)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn open_issues_page_with_the_raw_key_and_the_filter() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(header("authorization", KEY))
            .and(body_string_contains("issues(first"))
            .and(body_partial_json(json!({"variables": {"team": "ENG", "label": "provefab", "after": "c1"}})))
            .respond_with(data(json!({"issues": {"nodes": [node(2, "started")], "pageInfo": {"hasNextPage": false, "endCursor": "c2"}}})))
            .with_priority(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("issues(first"))
            .respond_with(data(json!({"issues": {"nodes": [node(1, "unstarted")], "pageInfo": {"hasNextPage": true, "endCursor": "c1"}}})))
            .expect(1)
            .mount(&server)
            .await;
        let issues = linear(&server)
            .open_issues("acme/web", "provefab")
            .await
            .unwrap();
        assert_eq!(issues.iter().map(|i| i.number).collect::<Vec<_>>(), [1, 2]);
        let i = &issues[0];
        assert_eq!(i.key.as_deref(), Some("ENG-1"));
        assert_eq!(
            (i.title.as_str(), i.body.as_str(), i.author.as_str()),
            ("Ticket 1", "Crash on **start**", "user-alice")
        );
        assert_eq!(i.url, "https://linear.app/acme/issue/ENG-1/ticket-1");
        assert_eq!(i.labels, ["provefab"]);
        let sent =
            String::from_utf8(server.received_requests().await.unwrap()[0].body.clone()).unwrap();
        assert!(
            sent.contains("nin")
                && sent.contains("completed")
                && sent.contains("canceled")
                && sent.contains("duplicate"),
            "{sent}"
        );
    }

    #[tokio::test]
    async fn open_issues_stop_at_the_cap() {
        // A new cursor each page: a repeated one would stop the listing first.
        let page: Vec<Value> = (1..=100).map(|n| node(n, "started")).collect();
        let (server, calls) = endless("issues", json!(page), false).await;
        let issues = linear(&server)
            .open_issues("acme/web", "provefab")
            .await
            .unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 10);
        assert_eq!(issues.len(), crate::forge::ISSUE_LIMIT);
        // The warning `open_issues` prints (to stderr, not capturable here).
        assert!(crate::forge::issue_limit_warning("acme/web", "provefab", issues.len()).is_some());
    }

    /// Answers `hasNextPage: true` forever, with a new cursor each time
    /// unless `same`, and counts the requests.
    struct Endless {
        /// `issues`, or `comments` of one issue.
        what: &'static str,
        nodes: Value,
        same: bool,
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl wiremock::Respond for Endless {
        fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
            let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let cursor = if self.same {
                "same".to_string()
            } else {
                format!("c{n}")
            };
            let page = json!({"nodes": self.nodes, "pageInfo": {"hasNextPage": true, "endCursor": cursor}});
            data(match self.what {
                "issues" => json!({"issues": page}),
                _ => json!({"issue": {"comments": page}}),
            })
        }
    }

    async fn endless(
        what: &'static str,
        nodes: Value,
        same: bool,
    ) -> (MockServer, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let server = MockServer::start().await;
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        Mock::given(method("POST"))
            .respond_with(Endless {
                what,
                nodes,
                same,
                calls: calls.clone(),
            })
            .mount(&server)
            .await;
        (server, calls)
    }

    async fn within_20s<T>(
        f: impl std::future::Future<Output = T>,
    ) -> Result<T, tokio::time::error::Elapsed> {
        tokio::time::timeout(std::time::Duration::from_secs(20), f).await
    }

    #[tokio::test]
    async fn paging_ends_on_a_repeated_cursor_or_at_the_page_cap() {
        use std::sync::atomic::Ordering::SeqCst;
        // Nodes of another team never count, so only the guards stop the loop.
        let foreign = json!([{"identifier": "OPS-1", "title": "t", "url": "u"}]);
        let (server, calls) = endless("issues", foreign.clone(), true).await;
        let issues = within_20s(linear(&server).open_issues("acme/web", "provefab"))
            .await
            .unwrap()
            .unwrap();
        assert!(issues.is_empty());
        assert_eq!(calls.load(SeqCst), 2, "the first repeat stops it");
        let (server, calls) = endless("issues", foreign, false).await;
        within_20s(linear(&server).open_issues("acme/web", "provefab"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(calls.load(SeqCst), ISSUE_PAGES);
        let (server, calls) = endless("comments", json!([]), true).await;
        within_20s(linear(&server).comments("acme/web", 7))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(calls.load(SeqCst), 2);
        let (server, calls) = endless("comments", json!([]), false).await;
        within_20s(linear(&server).comments("acme/web", 7))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(calls.load(SeqCst), COMMENT_PAGES);
    }

    #[tokio::test]
    async fn comments_follow_every_page() {
        let server = MockServer::start().await;
        let comment = |body: &str| json!({"body": body, "createdAt": "2026-10-01T08:00:00Z", "user": {"id": "user-bob"}});
        Mock::given(method("POST"))
            .and(body_partial_json(json!({"variables": {"after": "p1"}})))
            .respond_with(data(json!({"issue": {"comments": {"nodes": [comment("second")], "pageInfo": {"hasNextPage": false, "endCursor": "p2"}}}})))
            .with_priority(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(data(json!({"issue": {"comments": {"nodes": [comment("first")], "pageInfo": {"hasNextPage": true, "endCursor": "p1"}}}})))
            .expect(1)
            .mount(&server)
            .await;
        let comments = linear(&server).comments("acme/web", 7).await.unwrap();
        assert_eq!(
            comments.iter().map(|c| c.body.as_str()).collect::<Vec<_>>(),
            ["first", "second"]
        );
    }

    /// An issue answer without its id never sends a mutation with an empty or
    /// null id.
    #[tokio::test]
    async fn a_missing_issue_id_stops_before_the_mutation() {
        let server = MockServer::start().await;
        let mut issue = node(7, "started");
        issue.as_object_mut().unwrap().remove("id");
        on(
            &server,
            "issue(id: $id) { id identifier",
            data(json!({"issue": issue})),
        )
        .await;
        Mock::given(method("POST"))
            .and(body_string_contains("mutation"))
            .respond_with(data(json!({})))
            .expect(0)
            .mount(&server)
            .await;
        let l = linear(&server);
        let err = l.comment("acme/web", 7, "hi").await.unwrap_err();
        assert!(matches!(err, ForgeError::Parse(..)), "{err}");
        let err = l
            .edit_labels("acme/web", 7, &[], &["provefab"])
            .await
            .unwrap_err();
        assert!(matches!(err, ForgeError::Parse(..)), "{err}");
    }

    #[tokio::test]
    async fn ensure_label_creates_a_team_label_only_when_none_exists() {
        let server = MockServer::start().await;
        on(
            &server,
            "teams(",
            data(json!({"teams": {"nodes": [{"id": "team-1"}]}})),
        )
        .await;
        Mock::given(method("POST"))
            .and(body_string_contains("issueLabels("))
            .and(body_partial_json(json!({"variables": {"name": "provefab:failed"}})))
            .respond_with(data(json!({"issueLabels": {"nodes": [{"id": "l-failed", "name": "provefab:failed", "team": null}]}})))
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("issueLabels("))
            .respond_with(data(json!({"issueLabels": {"nodes": [{"id": "l-other-team", "name": "provefab:in-pr", "team": {"id": "team-9"}}]}})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("issueLabelCreate"))
            .and(body_partial_json(json!({"variables": {"input": {"name": "provefab:in-pr", "color": "#0e8a16", "teamId": "team-1"}}})))
            .respond_with(data(json!({"issueLabelCreate": {"success": true, "issueLabel": {"id": "l-new"}}})))
            .expect(1)
            .mount(&server)
            .await;
        let l = linear(&server);
        l.ensure_label(
            "acme/web",
            "provefab:in-pr",
            "0e8a16",
            "Provefab opened a pull request",
        )
        .await
        .unwrap();
        l.ensure_label("acme/web", "provefab:failed", "d93f0b", "Provefab stopped")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn edit_labels_adds_and_removes_by_id_without_replacing_the_set() {
        let server = MockServer::start().await;
        on(
            &server,
            "issue(id: $id) { id identifier",
            data(json!({"issue": node(7, "started")})),
        )
        .await;
        on(
            &server,
            "teams(",
            data(json!({"teams": {"nodes": [{"id": "team-1"}]}})),
        )
        .await;
        on(&server, "issueLabels(", data(json!({"issueLabels": {"nodes": [{"id": "l-in-pr", "name": "provefab:in-pr", "team": {"id": "team-1"}}]}}))).await;
        Mock::given(method("POST"))
            .and(body_string_contains("issueUpdate"))
            .and(body_partial_json(
                json!({"variables": {"id": "uuid-7", "added": ["l-in-pr"], "removed": ["l-trigger"]}}),
            ))
            .respond_with(data(json!({"issueUpdate": {"success": true}})))
            .expect(1)
            .mount(&server)
            .await;
        linear(&server)
            .edit_labels("acme/web", 7, &["provefab:in-pr"], &["provefab"])
            .await
            .unwrap();
        let updates: Vec<String> = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|r| String::from_utf8(r.body.clone()).unwrap())
            .filter(|b| b.contains("issueUpdate"))
            .collect();
        // A label a person adds meanwhile survives: the set is never replaced.
        assert!(
            updates.iter().all(|b| !b.contains("labelIds:")),
            "{updates:?}"
        );
    }

    #[tokio::test]
    async fn edit_labels_sends_nothing_when_there_is_nothing_to_change() {
        let server = MockServer::start().await;
        on(
            &server,
            "issue(id: $id) { id identifier",
            data(json!({"issue": node(7, "started")})),
        )
        .await;
        Mock::given(method("POST"))
            .and(body_string_contains("issueUpdate"))
            .respond_with(data(json!({"issueUpdate": {"success": true}})))
            .expect(0)
            .mount(&server)
            .await;
        let l = linear(&server);
        l.edit_labels("acme/web", 7, &[], &[]).await.unwrap();
        // Already there, and not there: no change to send.
        l.edit_labels("acme/web", 7, &["provefab"], &["provefab:failed"])
            .await
            .unwrap();
        let sent = server.received_requests().await.unwrap().len();
        assert_eq!(sent, 1, "only the read of the second call");
    }

    #[tokio::test]
    async fn comments_are_markdown_from_people_with_utc_times() {
        let server = MockServer::start().await;
        on(
            &server,
            "issue(id: $id) { id identifier",
            data(json!({"issue": node(7, "started")})),
        )
        .await;
        Mock::given(method("POST"))
            .and(body_string_contains("commentCreate"))
            .and(body_partial_json(json!({"variables": {"id": "uuid-7", "body": crate::forge::with_prefix("Which version?")}})))
            .respond_with(data(json!({"commentCreate": {"success": true}})))
            .expect(1)
            .mount(&server)
            .await;
        on(&server, "{ comments(first", data(json!({"issue": {"comments": {
            "nodes": [
                {"body": "_Posted by Provefab (automated), not typed by a person._\n\nWhich version?", "createdAt": "2026-10-01T07:30:00.000Z", "user": {"id": "user-op"}},
                {"body": "It is **v2**", "createdAt": "2026-10-01T08:00:00.123Z", "user": {"id": "user-bob"}},
                {"body": "Linked a PR", "createdAt": "2026-10-01T08:01:00.000Z", "user": null},
                {"body": "Agent summary", "createdAt": "2026-10-01T08:02:00.000Z", "user": {"id": "user-app", "app": true}}
            ],
            "pageInfo": {"hasNextPage": false, "endCursor": null}
        }}}))).await;
        let l = linear(&server);
        l.comment("acme/web", 7, "Which version?").await.unwrap();
        let comments = l.comments("acme/web", 7).await.unwrap();
        assert_eq!(
            comments.len(),
            2,
            "an integration's or an app's comment is not a person's"
        );
        assert!(comments.iter().all(|c| c.association == "MEMBER"));
        assert_eq!(comments[1].created_at, "2026-10-01T08:00:00Z");
        let replies =
            crate::intake::new_replies(&comments, "user-alice", Some("2026-10-01T07:30:00Z"));
        assert_eq!(
            replies.iter().map(|c| c.body.as_str()).collect::<Vec<_>>(),
            ["It is **v2**"]
        );
    }

    #[tokio::test]
    async fn issue_open_is_false_once_completed_or_canceled() {
        for (state, open) in [
            ("started", true),
            ("backlog", true),
            ("completed", false),
            ("canceled", false),
            ("duplicate", false),
        ] {
            let server = MockServer::start().await;
            on(&server, "issue(id:", data(json!({"issue": node(7, state)}))).await;
            assert_eq!(
                linear(&server).issue_open("acme/web", 7).await.unwrap(),
                open,
                "{state}"
            );
        }
    }

    #[tokio::test]
    async fn errors_in_the_status_or_the_body_are_classified_without_the_key() {
        let echo = |code: &str, message: &str| json!({"errors": [{"message": format!("{message} ({KEY})"), "extensions": {"code": code}}]});
        for (status, body, permanent) in [
            (401, json!({}), true),
            (
                200,
                echo("AUTHENTICATION_ERROR", "Authentication required"),
                true,
            ),
            (200, echo("FORBIDDEN", "Forbidden"), true),
            (200, echo("INVALID_INPUT", "Entity not found: Issue"), true),
            (400, echo("RATELIMITED", "Rate limit exceeded"), false),
            (500, json!({}), false),
            // The HTTP status wins when it says "try later", whatever the body.
            (503, echo("SOMETHING_NEW", "Service unavailable"), false),
            (429, echo("SOMETHING_NEW", "Slow down"), false),
            (200, echo("SOMETHING_NEW", "Unexpected"), true),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(status).set_body_json(body.clone()))
                .mount(&server)
                .await;
            let err = linear(&server).issue("acme/web", 7).await.unwrap_err();
            assert_eq!(err.is_permanent(), permanent, "{status} {body}: {err}");
            let shown = format!("{err} {err:?}");
            assert!(!shown.contains(KEY), "{shown}");
        }
    }

    /// The other ways a call fails: no answer, no data, no such issue, a
    /// mutation that did not succeed. None of them shows the key.
    #[tokio::test]
    async fn other_failures_never_show_the_key() {
        let offline = Linear::new("http://127.0.0.1:1", "ENG", KEY.into());
        let err = offline.issue("acme/web", 7).await.unwrap_err();
        assert!(!err.is_permanent(), "{err}");
        let mut shown = vec![format!("{err} {err:?}")];
        let server = MockServer::start().await;
        on(
            &server,
            "issue(id: $id) { id identifier",
            data(json!({"issue": null})),
        )
        .await;
        let err = linear(&server).issue("acme/web", 7).await.unwrap_err();
        assert!(err.is_permanent(), "{err}");
        shown.push(format!("{err} {err:?}"));
        let server = MockServer::start().await;
        on(
            &server,
            "issue(id: $id) { id identifier",
            data(json!({"issue": node(7, "started")})),
        )
        .await;
        on(
            &server,
            "commentCreate",
            data(json!({"commentCreate": {"success": false}})),
        )
        .await;
        let err = linear(&server)
            .comment("acme/web", 7, "hi")
            .await
            .unwrap_err();
        shown.push(format!("{err} {err:?}"));
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!("not json {KEY}")))
            .mount(&server)
            .await;
        let err = linear(&server).issue("acme/web", 7).await.unwrap_err();
        shown.push(format!("{err} {err:?}"));
        for s in shown {
            assert!(!s.contains(KEY), "{s}");
        }
    }

    #[tokio::test]
    async fn check_names_the_workspace_and_the_team() {
        let server = MockServer::start().await;
        on(&server, "viewer", data(json!({"viewer": {"id": "user-op", "organization": {"urlKey": "acme"}}, "teams": {"nodes": [{"id": "team-1"}]}}))).await;
        assert_eq!(
            linear(&server).check().await.unwrap(),
            "workspace acme; credentials accepted; team ENG readable"
        );
        let empty = MockServer::start().await;
        on(&empty, "viewer", data(json!({"viewer": {"id": "user-op", "organization": {"urlKey": "acme"}}, "teams": {"nodes": []}}))).await;
        assert!(linear(&empty).check().await.unwrap_err().is_permanent());
    }
}
