//! Jira Cloud as an issue tracker (issue trackers spec §6): REST API v3 with
//! an account e-mail and API token. Labels and comments only: Provefab never
//! changes a ticket's status.

use reqwest::Method;
use serde_json::{Value, json};

use crate::forge::{Comment, ForgeError, ISSUE_LIMIT, Issue, issue_limit_warning, with_prefix};
use crate::ports::Tracker;
use crate::tracker::{JiraAuth, http_client, http_error, number_of, send, utc_seconds};

const SERVICE: &str = "jira";
const FIELDS: &str = "summary,description,reporter,labels";

pub struct Jira {
    /// `https://<site>`; a test server's address in tests.
    pub api: String,
    /// The host name in ticket links, `acme.atlassian.net`.
    pub site: String,
    /// The project key: `ENG` in `ENG-123`.
    pub project: String,
    auth: JiraAuth,
    http: reqwest::Client,
}

/// The JQL Provefab builds (never the user): every value quoted and escaped,
/// so a label cannot add clauses (spec §6).
pub fn jql(project: &str, label: &str) -> String {
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    format!(
        "project = {} AND labels = {} AND statusCategory != Done ORDER BY created ASC",
        quote(project),
        quote(label)
    )
}

impl Jira {
    pub fn new(api: &str, site: &str, project: &str, auth: JiraAuth) -> Self {
        Self {
            api: api.trim_end_matches('/').to_string(),
            site: site.to_string(),
            project: project.to_string(),
            auth,
            http: http_client(),
        }
    }

    fn key(&self, number: u64) -> String {
        format!("{}-{number}", self.project)
    }

    fn url(&self, path: &str, query: &[(&str, &str)]) -> Result<reqwest::Url, ForgeError> {
        let mut url = reqwest::Url::parse(&format!("{}{path}", self.api))
            .map_err(|e| ForgeError::Parse("jira url".into(), e.to_string()))?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        Ok(url)
    }

    async fn call(
        &self,
        method: Method,
        url: reqwest::Url,
        body: Option<&Value>,
    ) -> Result<String, ForgeError> {
        let secrets = [self.auth.token.as_str(), self.auth.email.as_str()];
        let (status, retry_after, text) = send(SERVICE, &secrets, || {
            let r = self
                .http
                .request(method.clone(), url.clone())
                .basic_auth(&self.auth.email, Some(&self.auth.token))
                .header("Accept", "application/json");
            match body {
                Some(b) => r.json(b),
                None => r,
            }
        })
        .await?;
        if (200..300).contains(&status) {
            Ok(text)
        } else {
            Err(http_error(SERVICE, status, retry_after, &text, &secrets))
        }
    }

    async fn get_json(&self, path: &str, query: &[(&str, &str)]) -> Result<Value, ForgeError> {
        let text = self.call(Method::GET, self.url(path, query)?, None).await?;
        serde_json::from_str(&text).map_err(|e| ForgeError::Parse("jira".into(), e.to_string()))
    }

    fn issue_of(&self, v: &Value) -> Option<Issue> {
        let key = v["key"].as_str()?;
        let f = &v["fields"];
        Some(Issue {
            number: number_of(key, &self.project)?,
            key: Some(key.to_string()),
            title: f["summary"].as_str()?.to_string(),
            body: if f["description"].is_object() {
                from_adf(&f["description"])
            } else {
                String::new()
            },
            url: format!("https://{}/browse/{key}", self.site),
            // No reporter (an import, a deleted account): nobody answers as the author.
            author: f["reporter"]["accountId"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            labels: f["labels"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|l| l.as_str().map(str::to_string))
                .collect(),
        })
    }

    /// For `provefab doctor`: the credentials authenticate and the project is readable.
    pub async fn check(&self) -> Result<String, ForgeError> {
        self.get_json("/rest/api/3/myself", &[]).await?;
        self.get_json(&format!("/rest/api/3/project/{}", self.project), &[])
            .await?;
        Ok(format!(
            "credentials accepted; project {} readable",
            self.project
        ))
    }
}

/// A person's comment; an app's (`accountType = "app"`) is skipped (plan decision 4).
fn comment_of(c: &Value) -> Option<Comment> {
    if c["author"]["accountType"].as_str() == Some("app") {
        return None;
    }
    let created = c["created"].as_str()?;
    let Some(created_at) = utc_seconds(created) else {
        eprintln!("provefab: a jira comment with an unreadable time {created:?} is skipped");
        return None;
    };
    Some(Comment {
        author: c["author"]["accountId"].as_str()?.to_string(),
        // Only workspace members can comment on Jira (spec §8, decision 9).
        association: "MEMBER".into(),
        body: if c["body"].is_object() {
            from_adf(&c["body"])
        } else {
            c["body"].as_str().unwrap_or_default().to_string()
        },
        created_at,
    })
}

impl Tracker for Jira {
    async fn open_issues(&self, slug: &str, label: &str) -> Result<Vec<Issue>, ForgeError> {
        let jql = jql(&self.project, label);
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut query = vec![
                ("jql", jql.as_str()),
                ("fields", FIELDS),
                ("maxResults", "100"),
            ];
            if let Some(t) = &token {
                query.push(("nextPageToken", t.as_str()));
            }
            let v = self.get_json("/rest/api/3/search/jql", &query).await?;
            let page = v["issues"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default();
            out.extend(page.iter().filter_map(|i| self.issue_of(i)));
            let next = v["nextPageToken"].as_str().map(str::to_string);
            // An empty page that still names a next one would page forever.
            if page.is_empty()
                || out.len() >= ISSUE_LIMIT
                || next.is_none()
                || v["isLast"].as_bool() == Some(true)
            {
                break;
            }
            token = next;
        }
        out.truncate(ISSUE_LIMIT);
        if let Some(warning) = issue_limit_warning(slug, label, out.len()) {
            eprintln!("{warning}");
        }
        Ok(out)
    }

    async fn issue(&self, _slug: &str, number: u64) -> Result<Issue, ForgeError> {
        let v = self
            .get_json(
                &format!("/rest/api/3/issue/{}", self.key(number)),
                &[("fields", FIELDS)],
            )
            .await?;
        self.issue_of(&v).ok_or_else(|| {
            ForgeError::Parse(
                "jira issue".into(),
                format!("no key or summary for {}", self.key(number)),
            )
        })
    }

    async fn comments(&self, _slug: &str, number: u64) -> Result<Vec<Comment>, ForgeError> {
        let path = format!("/rest/api/3/issue/{}/comment", self.key(number));
        let mut out = Vec::new();
        let mut start = 0usize;
        loop {
            let at = start.to_string();
            let v = self
                .get_json(
                    &path,
                    &[
                        ("startAt", at.as_str()),
                        ("maxResults", "100"),
                        ("orderBy", "created"),
                    ],
                )
                .await?;
            let page = v["comments"].as_array().cloned().unwrap_or_default();
            out.extend(page.iter().filter_map(comment_of));
            start += page.len();
            if page.is_empty() || start >= v["total"].as_u64().unwrap_or(0) as usize {
                break;
            }
        }
        Ok(out)
    }

    async fn comment(&self, _slug: &str, number: u64, body: &str) -> Result<(), ForgeError> {
        let url = self.url(
            &format!("/rest/api/3/issue/{}/comment", self.key(number)),
            &[],
        )?;
        let doc = json!({"body": to_adf(&with_prefix(body))});
        self.call(Method::POST, url, Some(&doc)).await.map(|_| ())
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
        let ops: Vec<Value> = add
            .iter()
            .map(|l| json!({"add": l}))
            .chain(remove.iter().map(|l| json!({"remove": l})))
            .collect();
        let url = self.url(&format!("/rest/api/3/issue/{}", self.key(number)), &[])?;
        self.call(Method::PUT, url, Some(&json!({"update": {"labels": ops}})))
            .await
            .map(|_| ())
    }

    /// Jira labels are free text: there is nothing to create (spec §6).
    async fn ensure_label(&self, _: &str, _: &str, _: &str, _: &str) -> Result<(), ForgeError> {
        Ok(())
    }

    async fn issue_open(&self, _slug: &str, number: u64) -> Result<bool, ForgeError> {
        let v = self
            .get_json(
                &format!("/rest/api/3/issue/{}", self.key(number)),
                &[("fields", "status")],
            )
            .await?;
        match v
            .pointer("/fields/status/statusCategory/key")
            .and_then(Value::as_str)
        {
            Some(category) => Ok(category != "done"),
            None => Err(ForgeError::Parse(
                "jira issue".into(),
                "no status category".into(),
            )),
        }
    }
}

// ---------- Atlassian Document Format ----------

/// Provefab's Markdown as an ADF document (spec §6, decision 10): paragraphs
/// (lines joined by hard breaks), `- ` bullet lists, fenced code blocks, inline
/// code, links, `*emphasis*` and `**strong**`. Anything else stays plain text.
pub fn to_adf(markdown: &str) -> Value {
    let lines: Vec<&str> = markdown.lines().collect();
    let is_bullet = |l: &str| l.trim_start().starts_with("- ");
    let is_fence = |l: &str| l.trim_start().starts_with("```");
    let mut content = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.trim().is_empty() {
            i += 1;
        } else if is_fence(line) {
            let t = line.trim_start();
            let ticks = t.chars().take_while(|c| *c == '`').count();
            let lang = t[ticks..].trim();
            let close = "`".repeat(ticks);
            let mut code = Vec::new();
            i += 1;
            while i < lines.len() && lines[i].trim() != close {
                code.push(lines[i]);
                i += 1;
            }
            i += 1;
            let text = code.join("\n");
            let mut node = json!({"type": "codeBlock", "content": []});
            if !text.is_empty() {
                node["content"] = json!([{"type": "text", "text": text}]);
            }
            if !lang.is_empty() {
                node["attrs"] = json!({"language": lang});
            }
            content.push(node);
        } else if is_bullet(line) {
            let mut items = Vec::new();
            while i < lines.len() && is_bullet(lines[i]) {
                let text = &lines[i].trim_start()[2..];
                items.push(json!({"type": "listItem", "content": [{"type": "paragraph", "content": inline(text)}]}));
                i += 1;
            }
            content.push(json!({"type": "bulletList", "content": items}));
        } else {
            let mut para: Vec<Value> = Vec::new();
            while i < lines.len()
                && !lines[i].trim().is_empty()
                && !is_bullet(lines[i])
                && !is_fence(lines[i])
            {
                if !para.is_empty() {
                    para.push(json!({"type": "hardBreak"}));
                }
                para.extend(inline(lines[i]));
                i += 1;
            }
            content.push(json!({"type": "paragraph", "content": para}));
        }
    }
    json!({"version": 1, "type": "doc", "content": content})
}

fn inline(text: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let mut plain = String::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        let parsed = match c {
            '`' => span(rest, "`", "`", false).map(|(t, n)| (marked(t, "code"), n)),
            '[' => link(rest),
            '*' if rest.starts_with("**") => {
                span(rest, "**", "**", true).map(|(t, n)| (marked(t, "strong"), n))
            }
            '*' => span(rest, "*", "*", true).map(|(t, n)| (marked(t, "em"), n)),
            _ => None,
        };
        match parsed {
            Some((node, used)) => {
                if !plain.is_empty() {
                    out.push(json!({"type": "text", "text": std::mem::take(&mut plain)}));
                }
                out.push(node);
                rest = &rest[used..];
            }
            None => {
                plain.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    if !plain.is_empty() {
        out.push(json!({"type": "text", "text": plain}));
    }
    out
}

/// `open inner close` at the start of `s`: (inner, bytes used). `tight`: the
/// inner text neither starts nor ends with a space (so `2 * 3 * 4` stays plain).
fn span<'a>(s: &'a str, open: &str, close: &str, tight: bool) -> Option<(&'a str, usize)> {
    let body = s.strip_prefix(open)?;
    let end = body.find(close)?;
    let inner = &body[..end];
    let ok = !inner.trim().is_empty()
        && (!tight
            || (!inner.starts_with(char::is_whitespace) && !inner.ends_with(char::is_whitespace)));
    ok.then_some((inner, open.len() + end + close.len()))
}

fn link(s: &str) -> Option<(Value, usize)> {
    let (label, used) = span(s, "[", "](", false)?;
    let after = &s[used..];
    let end = after.find(')')?;
    let href = &after[..end];
    (href.starts_with("https://") || href.starts_with("http://")).then(|| {
        (
            json!({"type": "text", "text": label, "marks": [{"type": "link", "attrs": {"href": href}}]}),
            used + end + 1,
        )
    })
}

fn marked(text: &str, mark: &str) -> Value {
    json!({"type": "text", "text": text, "marks": [{"type": mark}]})
}

/// An ADF document as text: what agents read and what Provefab compares.
/// Code keeps its backticks and links their target; emphasis is dropped.
pub fn from_adf(doc: &Value) -> String {
    doc["content"]
        .as_array()
        .into_iter()
        .flatten()
        .map(block)
        .filter(|b| !b.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn children(node: &Value) -> impl Iterator<Item = &Value> {
    node["content"].as_array().into_iter().flatten()
}

fn block(node: &Value) -> String {
    match node["type"].as_str().unwrap_or_default() {
        "paragraph" => inline_text(node),
        "heading" => {
            let level = node["attrs"]["level"].as_u64().unwrap_or(1).clamp(1, 6) as usize;
            format!("{} {}", "#".repeat(level), inline_text(node))
        }
        "codeBlock" => format!(
            "```{}\n{}\n```",
            node["attrs"]["language"].as_str().unwrap_or_default(),
            inline_text(node)
        ),
        "bulletList" => children(node)
            .map(|i| format!("- {}", item_text(i)))
            .collect::<Vec<_>>()
            .join("\n"),
        "orderedList" => children(node)
            .enumerate()
            .map(|(n, i)| format!("{}. {}", n + 1, item_text(i)))
            .collect::<Vec<_>>()
            .join("\n"),
        "rule" => "---".into(),
        _ => {
            let inner: Vec<String> = children(node)
                .map(block)
                .filter(|b| !b.is_empty())
                .collect();
            if inner.is_empty() {
                inline_text(node)
            } else {
                inner.join("\n\n")
            }
        }
    }
}

fn item_text(item: &Value) -> String {
    children(item).map(block).collect::<Vec<_>>().join("\n  ")
}

fn inline_text(node: &Value) -> String {
    let mut s = String::new();
    for n in children(node) {
        match n["type"].as_str().unwrap_or_default() {
            "text" => {
                let text = n["text"].as_str().unwrap_or_default();
                let marks: Vec<&Value> = n["marks"].as_array().into_iter().flatten().collect();
                let code = marks.iter().any(|m| m["type"] == "code");
                let href = marks
                    .iter()
                    .find(|m| m["type"] == "link")
                    .and_then(|m| m["attrs"]["href"].as_str());
                match (code, href) {
                    (true, _) => s.push_str(&format!("`{text}`")),
                    (false, Some(h)) => s.push_str(&format!("[{text}]({h})")),
                    _ => s.push_str(text),
                }
            }
            "hardBreak" => s.push('\n'),
            "mention" | "emoji" | "status" | "date" => {
                s.push_str(n["attrs"]["text"].as_str().unwrap_or_default())
            }
            "inlineCard" => s.push_str(n["attrs"]["url"].as_str().unwrap_or_default()),
            _ => s.push_str(&inline_text(n)),
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::is_bot_comment;
    use crate::intake::new_replies;
    use wiremock::matchers::{
        body_json, header, method, path, query_param, query_param_is_missing,
    };
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SECRET: &str = "tok-SECRET-123";
    const EMAIL: &str = "bot@acme.test";

    fn jira(server: &MockServer) -> Jira {
        Jira::new(
            &server.uri(),
            "acme.atlassian.net",
            "ENG",
            JiraAuth {
                email: EMAIL.into(),
                token: SECRET.into(),
            },
        )
    }

    fn adf(text: &str) -> Value {
        json!({"version": 1, "type": "doc", "content": [{"type": "paragraph", "content": [{"type": "text", "text": text}]}]})
    }

    fn ticket(n: u64) -> Value {
        json!({"key": format!("ENG-{n}"), "fields": {
            "summary": format!("Ticket {n}"),
            "description": adf("Crash on start"),
            "reporter": {"accountId": "acc-alice"},
            "labels": ["provefab"]
        }})
    }

    #[test]
    fn jql_quotes_and_escapes_every_value() {
        assert_eq!(
            jql("ENG", "provefab"),
            r#"project = "ENG" AND labels = "provefab" AND statusCategory != Done ORDER BY created ASC"#
        );
        assert_eq!(
            jql("ENG", r#"pro"fab\x OR project = OPS"#),
            r#"project = "ENG" AND labels = "pro\"fab\\x OR project = OPS" AND statusCategory != Done ORDER BY created ASC"#
        );
    }

    #[test]
    fn adf_round_trip_keeps_what_provefab_compares() {
        let md = "*Posted by Provefab (automated), not typed by a person.*\n\nProvefab needs a person to continue.\n\nReason: the `gates` failed, see [the log](https://example.com/log).\n\n- one\n- `two`\n\n```sh\ncargo test\n```\n\n<!-- provefab-post-merge:3 -->";
        let text = from_adf(&to_adf(md));
        assert_eq!(
            text,
            "Posted by Provefab (automated), not typed by a person.\n\nProvefab needs a person to continue.\n\nReason: the `gates` failed, see [the log](https://example.com/log).\n\n- one\n- `two`\n\n```sh\ncargo test\n```\n\n<!-- provefab-post-merge:3 -->"
        );
        assert!(is_bot_comment(&text));
        assert!(text.contains(&crate::post_merge::marker(3)));
    }

    #[test]
    fn adf_marks_breaks_and_plain_stars() {
        let doc = to_adf("**bold** and *it*\nnext line");
        let para = &doc["content"][0]["content"];
        assert_eq!(para[0]["marks"][0]["type"], "strong");
        assert_eq!(para[2]["marks"][0]["type"], "em");
        assert_eq!(para[3]["type"], "hardBreak");
        let plain = to_adf("2 * 3 * 4");
        assert_eq!(
            plain["content"][0]["content"],
            json!([{"type": "text", "text": "2 * 3 * 4"}])
        );
        assert_eq!(to_adf("")["content"], json!([]));
        // Nodes Provefab never writes still read as text.
        let rich = json!({"type": "doc", "content": [
            {"type": "heading", "attrs": {"level": 2}, "content": [{"type": "text", "text": "Steps"}]},
            {"type": "orderedList", "content": [
                {"type": "listItem", "content": [{"type": "paragraph", "content": [
                    {"type": "mention", "attrs": {"text": "@Bob"}}, {"type": "text", "text": " runs it"}]}]}]},
            {"type": "panel", "content": [{"type": "paragraph", "content": [{"type": "inlineCard", "attrs": {"url": "https://x.test"}}]}]}
        ]});
        assert_eq!(
            from_adf(&rich),
            "## Steps\n\n1. @Bob runs it\n\nhttps://x.test"
        );
    }

    #[tokio::test]
    async fn open_issues_pages_with_the_token_and_builds_the_jql() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/search/jql"))
            .and(header(
                "authorization",
                "Basic Ym90QGFjbWUudGVzdDp0b2stU0VDUkVULTEyMw==",
            ))
            .and(query_param("jql", jql("ENG", "provefab")))
            .and(query_param("fields", "summary,description,reporter,labels"))
            .and(query_param_is_missing("nextPageToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"issues": [ticket(1)], "nextPageToken": "p2", "isLast": false}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/search/jql"))
            .and(query_param("nextPageToken", "p2"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"issues": [ticket(2)], "isLast": true})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let issues = jira(&server)
            .open_issues("acme/api", "provefab")
            .await
            .unwrap();
        assert_eq!(issues.iter().map(|i| i.number).collect::<Vec<_>>(), [1, 2]);
        let i = &issues[0];
        assert_eq!(i.key.as_deref(), Some("ENG-1"));
        assert_eq!(i.url, "https://acme.atlassian.net/browse/ENG-1");
        assert_eq!(
            (i.title.as_str(), i.body.as_str(), i.author.as_str()),
            ("Ticket 1", "Crash on start", "acc-alice")
        );
        assert_eq!(i.labels, ["provefab"]);
    }

    #[tokio::test]
    async fn open_issues_stop_at_the_cap() {
        let server = MockServer::start().await;
        let page: Vec<Value> = (1..=100).map(ticket).collect();
        Mock::given(method("GET"))
            .and(path("/rest/api/3/search/jql"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({"issues": page, "nextPageToken": "more", "isLast": false}),
                ),
            )
            .expect(10)
            .mount(&server)
            .await;
        let issues = jira(&server)
            .open_issues("acme/api", "provefab")
            .await
            .unwrap();
        assert_eq!(issues.len(), crate::forge::ISSUE_LIMIT);
    }

    #[tokio::test]
    async fn labels_are_added_and_removed_in_one_update_and_never_created() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/rest/api/3/issue/ENG-7"))
            .and(body_json(
                json!({"update": {"labels": [{"add": "provefab:in-pr"}, {"remove": "provefab"}]}}),
            ))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let j = jira(&server);
        j.edit_labels("acme/api", 7, &["provefab:in-pr"], &["provefab"])
            .await
            .unwrap();
        j.ensure_label("acme/api", "provefab:failed", "d93f0b", "x")
            .await
            .unwrap();
        j.edit_labels("acme/api", 7, &[], &[]).await.unwrap();
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn comments_are_adf_both_ways_with_members_and_utc_times() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/rest/api/3/issue/ENG-7/comment"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "1"})))
            .expect(1)
            .mount(&server)
            .await;
        let ours = to_adf(&crate::forge::with_prefix("Which version?"));
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/ENG-7/comment"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "startAt": 0, "maxResults": 100, "total": 4,
                "comments": [
                    {"author": {"accountId": "acc-op", "accountType": "atlassian"}, "body": ours, "created": "2026-10-01T09:30:00.000+0200"},
                    {"author": {"accountId": "acc-bob", "accountType": "atlassian"}, "body": adf("It is v2"), "created": "2026-10-01T10:00:00.000+0200"},
                    {"author": {"accountId": "acc-app", "accountType": "app"}, "body": adf("Build passed"), "created": "2026-10-01T10:01:00.000+0200"},
                    {"author": {"accountId": "acc-bob", "accountType": "atlassian"}, "body": adf("earlier"), "created": "2026-10-01T09:00:00.000+0200"}
                ]
            })))
            .mount(&server)
            .await;
        let j = jira(&server);
        j.comment("acme/api", 7, "Which version?").await.unwrap();
        let sent: Value = server.received_requests().await.unwrap()[0]
            .body_json()
            .unwrap();
        assert_eq!(sent["body"]["type"], "doc");
        assert_eq!(
            sent["body"]["content"][0]["content"][0]["marks"][0]["type"],
            "em"
        );
        let comments = j.comments("acme/api", 7).await.unwrap();
        assert_eq!(comments.len(), 3, "the app's comment is not a person's");
        assert!(comments.iter().all(|c| c.association == "MEMBER"));
        assert_eq!(comments[1].created_at, "2026-10-01T08:00:00Z");
        // The question was asked at 07:30 UTC: only Bob's later reply counts.
        let replies = new_replies(&comments, "acc-alice", Some("2026-10-01T07:30:00Z"));
        assert_eq!(
            replies.iter().map(|c| c.body.as_str()).collect::<Vec<_>>(),
            ["It is v2"]
        );
    }

    #[tokio::test]
    async fn issue_and_issue_open_read_one_ticket() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/ENG-7"))
            .and(query_param("fields", "summary,description,reporter,labels"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ticket(7)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/ENG-7"))
            .and(query_param("fields", "status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"key": "ENG-7", "fields": {"status": {"statusCategory": {"key": "done"}}}}),
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/ENG-8"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"key": "ENG-8", "fields": {"status": {"statusCategory": {"key": "indeterminate"}}}})))
            .mount(&server)
            .await;
        let j = jira(&server);
        assert_eq!(
            j.issue("acme/api", 7).await.unwrap().key.as_deref(),
            Some("ENG-7")
        );
        assert!(!j.issue_open("acme/api", 7).await.unwrap());
        assert!(j.issue_open("acme/api", 8).await.unwrap());
    }

    #[tokio::test]
    async fn errors_are_permanent_or_transient_and_never_hold_credentials() {
        let echo = json!({"errorMessages": [format!("nothing for {EMAIL} {SECRET}")]});
        for (n, status, retry, permanent) in [
            (1u64, 401u16, None, true),
            (2, 403, None, true),
            (3, 404, None, true),
            (4, 500, None, false),
            (5, 429, Some("120"), false),
        ] {
            let server = MockServer::start().await;
            let mut answer = ResponseTemplate::new(status).set_body_json(echo.clone());
            if let Some(r) = retry {
                answer = answer.insert_header("Retry-After", r);
            }
            Mock::given(method("GET"))
                .respond_with(answer)
                .mount(&server)
                .await;
            let err = jira(&server).issue("acme/api", n).await.unwrap_err();
            assert_eq!(err.is_permanent(), permanent, "{status}: {err}");
            let shown = format!("{err} {err:?}");
            assert!(!shown.contains(SECRET) && !shown.contains(EMAIL), "{shown}");
        }
    }

    #[tokio::test]
    async fn an_empty_page_ends_the_search() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/search/jql"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({"issues": [], "nextPageToken": "again", "isLast": false}),
                ),
            )
            .expect(1)
            .mount(&server)
            .await;
        let issues = jira(&server)
            .open_issues("acme/api", "provefab")
            .await
            .unwrap();
        assert!(issues.is_empty());
    }

    #[tokio::test]
    async fn network_and_parse_errors_never_hold_credentials() {
        // Nothing listens on port 1: the call fails before any answer.
        let down = Jira::new(
            "http://127.0.0.1:1",
            "acme.atlassian.net",
            "ENG",
            JiraAuth {
                email: EMAIL.into(),
                token: SECRET.into(),
            },
        );
        let err = down.issue("acme/api", 7).await.unwrap_err();
        assert!(!err.is_permanent(), "{err}");
        assert!(err.to_string().starts_with("jira: network: "), "{err}");
        let shown = format!("{err} {err:?}");
        assert!(!shown.contains(SECRET) && !shown.contains(EMAIL), "{shown}");
        // A success whose body is not JSON, echoing what was sent.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(format!("<html>{EMAIL} {SECRET}</html>")),
            )
            .mount(&server)
            .await;
        let err = jira(&server).issue("acme/api", 7).await.unwrap_err();
        let shown = format!("{err} {err:?}");
        assert!(!shown.contains(SECRET) && !shown.contains(EMAIL), "{shown}");
    }

    #[tokio::test]
    async fn a_short_retry_after_is_waited_out_once() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({"fields": {"status": {"statusCategory": {"key": "new"}}}}),
                ),
            )
            .mount(&server)
            .await;
        assert!(jira(&server).issue_open("acme/api", 7).await.unwrap());
    }

    #[tokio::test]
    async fn check_reads_the_account_and_the_project() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/myself"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"accountId": "acc-op"})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/project/ENG"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"key": "ENG"})))
            .mount(&server)
            .await;
        assert_eq!(
            jira(&server).check().await.unwrap(),
            "credentials accepted; project ENG readable"
        );
        let other = Jira::new(
            &server.uri(),
            "acme.atlassian.net",
            "OPS",
            JiraAuth {
                email: EMAIL.into(),
                token: SECRET.into(),
            },
        );
        assert!(other.check().await.unwrap_err().is_permanent());
    }
}
