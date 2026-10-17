# Config format

`config.json` (in `%APPDATA%\VirtualAudioMixer`) carries the version of its
format as `version`. Each version's format is described here as a JSON
Schema, `v<version>.schema.json`, generated from the config types: every
field, its type, its allowed values and its default.

When the mixer reads a config of an older version, it migrates it step by
step to its own (`MIGRATIONS` in `src/config.rs`) before reading it. A config
with no `version`, from 0.5.0 or earlier, is version 1. One from a newer
build is read as far as this build understands it: settings it doesn't know
are left out, so it first copies the file to `config.v<version>.json`.

A released version's schema never changes; the first change to the format
after a release makes a new version. See AGENTS.md.
