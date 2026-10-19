# LiteMotion project format (version 1)

A project is one UTF-8 JSON file with the extension `.lmproj`. Media is never
embedded; the file only references it.

Top-level keys: `format_version`, `name`, `width`, `height`, `fps`, `next_id`,
`media[]`, `tracks[]`, `export`.

- `media[]`: `id`, `kind` (`video|audio|image`), `path` (absolute), `rel_path`
  (relative to the project file, `/`-separated, may be absent), `duration_us`,
  `width`, `height`, `fps`, `has_audio`, `audio_channels`, `size_bytes`,
  `proxy_path`.
- `tracks[]`: `id`, `kind` (`video|overlay|text|audio`), `name`, `visible`,
  `muted`, `clips[]`.
- `clips[]`: `id`, `content` (`media`, `text`, `shape` or `solid`), `start`,
  `duration`, `source_in` (all microseconds), `speed`, `x`/`y` (fractions of the
  canvas, 0 = centre), `scale`, `rotation` (degrees), `opacity`, `volume`,
  `muted`, `fade_in`, `fade_out`, `fit`, `crop`, `color`, `transition_in`,
  `transition_out`, `keyframes[]`.
- `keyframes[]`: `prop` (`x|y|scale|rotation|opacity|volume`) and `keys[]` of
  `{t, v, interp}` where `t` is microseconds from the clip start and `interp` is
  `linear|ease_in|ease_out|ease_in_out`.

## Time
All times are integer microseconds. Frame positions are derived from `fps`.

## Moving a project
`resolve_media` tries `rel_path` (relative to the folder holding the `.lmproj`)
first, then the absolute `path`. A project saved with its media in a sub-folder
(for example with "Copy files into the project folder") can be moved to another
folder or computer as a whole. Media that can't be found opens the relink
dialog. Paths containing `://` are rejected.

## Safety
Project files are untrusted input: they are validated after loading
(`Project::validate`), nothing in them is executed, and FFmpeg is run with a
protocol whitelist of `file,pipe` only.

## Saving and recovery
Saves are written to a temporary file, flushed, then renamed over the target;
the previous good copy is kept as `<name>.lmproj.bak` and is used automatically
if the main file is damaged. Autosave writes to the app data folder (not over
your project) and is offered back after an unexpected exit.

## Versioning
`format_version` is checked on load. Newer files are refused with a clear
message. Older files are upgraded one version at a time by `migrate_step` in
`crates/engine/src/project/io.rs`. Version 1 is the first format, so there are
no migration steps yet. When the format changes: bump `FORMAT_VERSION`, add the
step, and add a fixture of the old version to the tests.
