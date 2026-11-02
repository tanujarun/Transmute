# Transmute

A personal, offline file converter for Windows, modelled on Tangerine for Mac. Drag a file, press Alt, and drop it on a format.

## Setup

```powershell
powershell -ExecutionPolicy Bypass -File scripts\fetch-tools.ps1
cargo build --release -p transmute
```

`fetch-tools.ps1` puts portable copies of the conversion tools in `tools\`. Nothing is installed system-wide. The app finds `tools\` by walking up from its executable, or through the `TRANSMUTE_TOOLS` environment variable. FFmpeg is copied from your existing install, so it must be on `PATH` when the script runs.

Run `target\release\transmute.exe`. It lives in the tray, and on first run it registers itself to start with Windows. The tray menu turns that off.

## Using it

1. Start dragging one or more files from Explorer, the Desktop, or any app that drags real files.
2. Press Alt, either a quick tap or held. The wheel opens at the cursor and stays open until you drop.
3. Drop on a format. The new file appears beside the original, and Explorer opens with it selected (skipped on the Desktop).

Scrolling while the wheel is open switches between Convert, Tools and Actions, with or without Alt held. Only wheels with something to offer appear, so a folder opens on Actions. Dropping on the centre does nothing. Dropping outside the wheel is a normal drop. Esc cancels.

Originals are never changed. Names never collide: `photo.png` becomes `photo (1).png`. Multi-page results such as PDF to PNG go in their own folder.

## Wheels

- **Convert:** images, video, audio, PDF, documents, slides, spreadsheets, subtitles and archives.
- **Tools, one click:** Compress, Mute, Mono, Stereo, Extract, Read QR, Split PDF, PDF/A and Page EPUB, plus Merge, Make PDF and Join for several files.
- **Tools, editor window:** To Size and Metadata for most files; photo Edit, Annotate, Add BG, Crop, Resize, Redact and Collage; video Trim, Crop, Speed, Frames, Split and Redact; audio Trim, Normalize, Channels, Bleep and Visualize; PDF Organize. Redact can find faces and text with the detectors built into Windows.
- **Actions:** Share, Float, Copy, Move, Copy Path, Terminal.

## Layout

| Path | Purpose |
|---|---|
| `crates/gesture` | Input hooks, the radial overlay and the OLE drop target. `render.rs` is portable; the rest is Windows-only. |
| `crates/core` | The format table, conversion routes, file tools and edits. Portable apart from the Office automation. |
| `crates/app` | Tauri tray app, job queue, progress, editor and Float windows, Windows integration. Share and detection run as helper processes (`--share`, `--detect`). |

## Tests

```powershell
cargo test --workspace
cargo test -p transmute-core --release --test matrix -- --ignored --nocapture
cargo test -p transmute-core --release --test filetools -- --ignored
cargo test -p transmute-core --release --test edits -- --ignored
cargo test -p transmute-core --release --test image_edits -- --ignored
cargo test -p transmute-core --release --test extras -- --ignored
cargo test -p transmute-core --lib office -- --ignored
```

The matrix runs every conversion route and validates each output. The next four measure the tools and editors. The last needs Microsoft Office.

Set `TRANSMUTE_LOG=1` before launching to trace the gesture and every job on stderr. Testing the gesture itself means dragging by hand.

## Limits

- PDF to PPTX and PDF to XLSX are best effort, because PDFs store positioned text rather than slides or tables.
- Apple Pages files are not supported.
- CSV and TSV exports take the first sheet of a workbook.
- HEIC output flattens transparency to white.
- Photo edits of HEIC and camera RAW files save as JPG.
- Photo Edit's live preview is an approximation of the saved result.

## Settings

The tray menu opens Settings: the wheel's trigger key (Alt, Right Alt, Ctrl or Shift), the terminal, Explorer behaviour, autostart, the tray icon and a finish sound. With the tray icon hidden, launching `transmute.exe` again opens Settings in the running copy.

## Differences from Tangerine

- AirDrop is the Windows share panel.
- Apple Pages is not supported.
- Metadata can be read and written for JPG, PNG, WebP, TIFF, PDF, video and audio. Other images can only have it removed.
- There is no paywall or file limit.