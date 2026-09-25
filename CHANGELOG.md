# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/jkellz-dev/mdpreviewer/releases/tag/v0.1.0) - 2026-09-25

### Added

- only act on Markdown files, and say so when refusing
- add --quit and --restart
- *(client)* open external links in a new tab
- *(client)* zoom a diagram, image, table or code block to fill the window
- *(client)* scroll to and highlight the editor's cursor line
- *(cli)* reuse a running server; add --line, --sync and --no-open
- *(server)* switch documents and scroll tabs via control requests
- *(server)* send named reload/scroll SSE events
- *(control)* add socket transport with stale-socket and race handling
- *(control)* add socket path and line protocol for the control channel
- *(render)* emit data-sourcepos on rendered blocks
- render front matter as a collapsed YAML block
- live markdown + mermaid preview server

### Changed

- rename the package and binary to mdpreviewer

### Documentation

- document zooming, --quit/--restart and the Helix setup
- bind the preview to \ m m and a scroll-only \ m r
- document scroll sync, server reuse and the Helix bindings
- add implementation plan for Helix cursor sync
- add design spec for Helix cursor sync and server reuse
- add CLAUDE.md with project context and architecture

### Fixed

- *(client)* zoom a whole mermaid block, and never zoom a link
- *(client)* do not select text while panning a zoomed block
- expand a leading ~ in file arguments
- *(test)* keep TestDir paths inside the sun_path limit
- reopen reliably after closing the tab; fall back on unusable sockets
