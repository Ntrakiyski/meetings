# Changelog

All notable changes to this fork are documented here.

## Unreleased

### Added

- Bulgarian-only recording now uses the local Whisper Large V3 Compressed model with Bulgarian fixed as the source language.
- Recording language controls now support Bulgarian-only, English-only, and bilingual Bulgarian + English modes.

### Changed

- Meeting audio remains AAC-LC in MP4 but is now encoded at 96 kbps instead of 192 kbps, reducing typical storage to about 43 MB per hour.
