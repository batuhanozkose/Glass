# Glass Radius Test Theme

This directory is a local dev extension for validating `component_radius`
theme overrides without bundling a custom theme into the app.

## Install locally

1. Open the Extensions page in Glass/Zed.
2. Run `Install Dev Extension`.
3. Select this `extensions/glass-theme` directory.

## Purpose

- Keep bundled Zed themes unchanged.
- Provide an explicit opt-in test theme for radius overrides.
- Mirror the extension shape that upstream expects: `extension.toml` plus
  `themes/`.
