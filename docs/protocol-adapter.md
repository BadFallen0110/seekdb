# Protocol Adapter Notes

The official `2026-competition-round2` driver had not been published when this
adapter was implemented. The current protocol is therefore a local compatibility
target, not a claim of official conformance.

## Current input

- SQL statements are separated by semicolons.
- String literals, backtick identifiers, line comments, and block comments are
  respected while finding statement boundaries.
- `QUIT`, `EXIT`, `SHUTDOWN`, and `\q` end the process.
- A standalone `mode=ann` or `mode=exact` line changes query mode.
- SQL comments containing `mode=ann` or `mode=exact` change query mode before
  that statement executes.

## Current output

- Startup: one configurable ready line, default `READY`.
- Query:

```text
BEGIN_RESULT
COLUMNS	column_a	column_b
ROW	value_a	value_b
END_RESULT
```

- Durable or in-memory DML: `AFFECTED<TAB>count`.
- DDL, transaction control, and zero-row DML: `OK`.
- Error: `ERROR<TAB>code<TAB>category<TAB>escaped-message`.
- Tab, newline, carriage return, and backslash are escaped with a backslash.

## Official-pack integration points

1. Change readiness and suppression rules in `ProtocolConfig` and the first
   write in `protocol::run`.
2. Change result framing only in `write_result`.
3. Change error numbers and text only in `error_code`, `category_name`, and
   `write_error`.
4. Change statement boundaries in `extract_statements`.
5. Change exact/ANN selection in `main` and `apply_comment_mode_hint`.
6. Change graceful shutdown markers in `is_exit_command`.

Database execution, WAL framing, transaction semantics, and vector calculations
must not depend on stdout formatting.
