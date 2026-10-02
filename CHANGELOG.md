# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.3](https://github.com/jkellz-dev/mdpreviewer/compare/v0.1.2...v0.1.3) - 2026-10-02

### Added

- *(typst)* scroll to, zoom and style pages
- *(typst)* reload when a file the document reads changes
- *(typst)* preview Typst documents
- *(typst)* tag where each source line lands on its page
- *(typst)* compile documents to SVG pages
- follow relative Markdown links and load relative images

### Documentation

- *(typst)* document the preview and add an example

### Fixed

- *(watch)* ignore events for files that have not changed since the watch began
- *(typst)* close gaps from the review
- close gaps from the review of the control-socket work

## [0.1.2](https://github.com/jkellz-dev/mdpreviewer/compare/v0.1.1...v0.1.2) - 2026-09-25

### Documentation

- *(readme)* add an installation section for cargo and cargo-binstall

### Fixed

- *(ci)* attach release binaries again by disabling release immutability

## [0.1.1](https://github.com/jkellz-dev/mdpreviewer/compare/v0.1.0...v0.1.1) - 2026-09-25

### Added

- *(cli)* answer --help and --version

### Fixed

- declare 1.88 as the minimum supported rust version
