---
name: sqlite-workflow
description: Run a SQLite smoke test using a host-provided database.
---

# Host Provider SQLite Demo

The host must register the SQLite provider before creating the engine. Discover the `sqlite-smoke` entry with `list_entries` and call its canonical name with `call_skill`. Pass an optional `note` string to write into the host-managed database.
