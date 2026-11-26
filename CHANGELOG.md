# Changelog

## Rust port

- Replaced the Go runtime with Rust while retaining the `flowerss-bot` binary, CLI flags, configuration, Telegram commands, OPML, and existing SQLite/MySQL records.
- Switched source builds, containers, CI, and release packaging to Cargo.
- Use dedicated feed and OPML parsers, typed Telegram Bot API requests, and bundled SQLite.
- Publish Linux amd64 and arm64 Docker images to GitHub Container Registry on `master` and version-tag pushes.
- Send all RSS updates as Telegram rich messages, with excerpts and collapsible articles. Remove Telegraph, custom push templates, and `-testtpl`.
- Use SQLx for both SQLite and MySQL, sharing queries and transactions without changing existing databases.

## [Unreleased] 2019-10-29

### Added
- source update fetch control by `/set` command (usually used when source updating paused by reached 100 error when 
getting update data)
- export feeds

### Changed
- merge `/set` command's response message template to a function for more struct

