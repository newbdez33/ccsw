# Tutorial video production

[Back to development](development.md)

Build ccsw tutorials from product UI excerpts, a HyperFrames timeline, and local
Qwen narration. Keep the script, specification, and delivery record in this
repository. Keep the editable media project and rendered files in a separate
archive, identified by a checksum in the production record.

The [Claude Code tutorial record](https://github.com/newbdez33/ccsw/blob/main/docs/specs/2026-11-03-ccsw-claude-tutorial.md) gives
the versions, dimensions, timing, and archive location used on 2026-11-03.
The commands below run from the extracted `ccsw-claude-tutorial/` directory.
The repository alone does not contain the renderer or its media assets.

## Establish the product behavior

Check the script against the selected ccsw revision and [CLI guide](usage.md).
Record the ccsw and provider CLI versions before capturing UI.

For this lesson, terminal A runs Claude Code. Terminal B runs ccsw shell
commands. Save the first login, use `/login` inside Claude Code for the second
account, complete browser authorization, return to the terminal success message,
press Enter, and only then save the second login. Keep this distinction visible
in the labels and narration.

Use the account slots shown in the example roster. Slots are shared across
providers. Usage percentages mean used quota. Explain optional fields only when
they appear, and preserve the Keychain refresh delay in the note. The lesson
ends with saving an existing Codex login; it does not teach new Codex sign-in.

## Prepare UI sources

Export ccsw views from its Ratatui components with in-memory example data. The
recorded project includes `assets/ui/fixtures.json`, so rendering does not need
the original fixture generator or access to saved accounts.

Capture Claude Code's native welcome, prompt, and command completion in an
isolated configuration. Preserve its terminal colors and layout when exporting
ANSI cells. Enlarge the welcome and input regions separately. Put explanatory
account labels outside the native UI.

Use actual browser login and completion-page excerpts. Keep OAuth URLs,
credentials, cookies, and private account data out of the project archive.
Mark reconstructed status panels as demonstrations, and record their text source.
A public completion page opened directly is not evidence of a completed login.

## Maintain the script and timeline

| File in the project | Purpose |
| --- | --- |
| `timeline-data.json` | Scene boundaries, caption text, and `cues[].spoken_text` |
| `assets/audio/timing.json` | Voice-aligned cue timing used by the scene builder |
| `assets/script/captions-original.zh-CN.srt` | Initial timing, used only when the voice timing file is absent |
| `captions-preview.zh-CN.srt` | Final subtitles aligned to the generated voice |
| `build_video.py` | Scene layout, UI placement, and animation rules |
| `index.html`, `compositions/` | Generated HyperFrames composition |

Edit spoken text in `timeline-data.json`, regenerate the affected voice cues,
then rebuild the scenes. `build_video.py` reads the audio timing when it exists
and rewrites the generated HTML and timeline data. A build before voice
regeneration can overwrite edits with the previous timing file. Save direct
HTML edits before running the builder, or put the change in the builder itself.

Use paused GSAP timelines so seeking produces the same frame. The project uses
the HyperFrames `code-terminal-run` component for command reveal and
`agent-cursor` for pointer feedback. Keep completed commands still long enough
to read, and outline the source row during each field explanation.

## Generate narration

Rendering the included audio needs Node.js 22 or newer, Chrome, and FFmpeg.
Voice regeneration also needs Apple Silicon, Python 3.12, MLX Audio 0.5.2,
and the cached Qwen model revisions listed in the production record.

The generator creates an original synthetic narrator with VoiceDesign, then
uses the Base model and `assets/audio/qwen/voice-reference.wav` for all cues.
It runs with the Hugging Face cache offline. Model weights are not in the ZIP.

```bash
python3 generate_audio_qwen.py
python3 build_video.py
npm run check
```

The generator reuses `assets/audio/qwen/cue-NN.wav` if it exists. Move the old
file into a revision backup before regenerating changed speech. If cues are
inserted, removed, or reordered, invalidate all affected numbered files from
the first changed index onward. Otherwise an old recording can be assigned to
new text. Keep the synthetic reference unless the narrator must change.

Each slot is at least the speech duration plus 0.5 seconds. The generator adds
0.12 seconds before speech, trims leading and trailing silence at -45 dB,
mixes at 48 kHz, and normalizes with
`loudnorm=I=-16:TP=-1.5:LRA=11`. Longer speech extends the timeline. Recheck all
later scene boundaries after a voice change.

The optional `review_audio_asr.py` script in the local working project sends
16 kHz mono PCM to a local Qwen3-ASR WebSocket at `ws://127.0.0.1:10098`.
The delivered ZIP contains its result, `qwen-asr-review.json`, but not that
script. Compare transcripts after normalizing punctuation, spacing, case, and
equivalent account spellings. ASR checks words; also listen for pronunciation,
cadence, and unwanted sounds before the next release.

## Preview and render

The archive pins HyperFrames 0.8.226 in `package.json`.

```bash
npm run dev
npm run check
npm run render -- --quality looks --fps 30 -o renders/ccsw-render.mp4
```

Use the final narration WAV when producing the delivery file. This also keeps
the video stream unchanged if an audio remux is needed:

```bash
ffmpeg -i renders/ccsw-render.mp4 -i assets/audio/narration.wav \
  -map 0:v:0 -map 1:a:0 -c:v copy -c:a aac -ar 48000 -ac 2 \
  -movflags +faststart -shortest renders/ccsw-claude-qwen-preview.mp4
```

Back up an existing delivery file before replacing it. Keep the MP4, cover,
caption file, and source ZIP from the same revision. Re-rendering is not
guaranteed to reproduce the published file byte for byte.

For remote preview, the original local project has a Tailscale server with an
allowlist for the page, video, poster, and ZIP. It supports byte ranges and
`Cache-Control: no-store`. Serve only those reviewed files, bind to the host's
Tailscale address, and change the media version when replacing a render.
The preview server and its address are local delivery details, not part of
the editable ZIP or a permanent public hosting service.

## Validate the delivery

Inspect the opening, each scene, each field focus, both sides of an account
switch, and every scene boundary. Review at phone width. Pay special attention
to the return from browser completion to terminal success and then account
storage. Check the cover in the platform's crop preview, separately from the
vertical video player.

```bash
ffprobe -v error -show_entries \
  format=duration,size:stream=codec_name,codec_type,width,height,pix_fmt,r_frame_rate,sample_rate,channels,nb_frames \
  -of json renders/ccsw-claude-qwen-preview.mp4
ffmpeg -v error -i renders/ccsw-claude-qwen-preview.mp4 -f null -
ffmpeg -hide_banner -i renders/ccsw-claude-qwen-preview.mp4 \
  -af loudnorm=I=-16:TP=-1.5:LRA=11:print_format=json -f null -
shasum -a 256 renders/ccsw-claude-qwen-preview.mp4
```

Check every narration segment fits its caption slot. Review checker warnings
individually; record accepted warnings and their reason. Separate measured
results, visual review, listening review, and checks that remain incomplete.

## Publish and archive

1. Upload the reviewed MP4 to the intended Xiaohongshu account.
2. Set the cover crop and verify that the heading and key action remain visible.
3. Paste the title and body with paragraph breaks. Select the required topics
   from the platform topic picker so they become topic entries.
4. Set the AI-generated-content label for synthetic narration and animation.
   Verify the audience, activity association, title, and note preview.
5. Publish after the user has authorized publication. Do not treat an upload
   or a filled form as a successful publication.
6. Check the success result and note manager. Record review status separately
   from public availability, save a screenshot, and close temporary browser tabs.

Archive the specification, final scene timing, final post, subtitle track,
source versions, media checksums, and publication result. Keep the MP4 and the
editable ZIP outside Git. Package only the required source files and reviewed
assets; exclude raw authentication captures, configuration directories, model
weights, dependency caches, and build outputs. Keep font and component licenses.

Treat earlier scripts and review notes as revision records. A final production
record must state which version was published and supersede outdated delivery
status in earlier notes.
