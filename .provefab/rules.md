Conventions for the Provefab core. Provefab reads this file from `main` and gives these rules to every task on this repository.

## R1: No em-dashes in user-facing text
sources: owner's writing convention

Docs, the README, CLI help and messages, error texts, and comments Provefab posts on GitHub never contain an em-dash. Use a comma, a colon, parentheses or a full stop instead.

## R2: The public core never names Provefab Pro's internals
sources: crates/provefab/tests/no_paid_code.rs

Never write, anywhere in this repository (code, tests, docs, plans, specs), the names that `crates/provefab/tests/no_paid_code.rs` searches for, and never describe how Provefab Pro implements its features. Mention Pro only by what it offers, and keep its internals in its own repository.

## R3: Secrets never reach errors, logs, Debug output or stored data
paths: crates/**

Tokens, API keys, account e-mails used as credentials and Authorization header values never appear in an error message, a log line, a `Debug` implementation, a stored output or an event. Redact before truncating, return fixed error text, and keep raw tool output in the local log only, redacted. Add a test that injects a secret-looking value and asserts it is absent.

## R4: Published migrations are never edited
paths: crates/provefab/migrations/**, crates/provefab/src/store.rs

A migration that has been released is append-only history: add a new numbered file instead of changing an existing one, and pin the new file's SHA-384 in `migrations_are_frozen`.

## R5: A behaviour change comes with a test that failed first, and with its user docs
paths: crates/**

Every change in behaviour adds or updates a test that fails without the change. When the change is visible to a user (a command, an output, a configuration key, a message), update the matching page in `docs/guide/` in the same change. Touch the README only when its quick start or its tables (commands, labels, day-to-day actions) change; never add detail sentences there. Never write a test that checks the wording of documentation.

## R6: No guarantees in user-facing text
sources: product wording rule

User-facing text never claims that Provefab proves, guarantees or makes code safe, secure or correct. Describe what was checked and by what (your commands, a reviewer from another provider), not an outcome it cannot promise.
