# Claude Code account tutorial production record

[Production workflow](../video-production.md)

Revision 5 teaches how to save multiple Claude Code logins, switch accounts,
and read ccsw account fields. Codex appears only in the closing section.
Xiaohongshu accepted the publication on 2026-11-03; the note manager showed
**under review** at the last check. That result does not establish public
availability or subsequent review status.

## Source versions

| Component | Recorded version |
| --- | --- |
| ccsw | v0.9.5, commit `dad3728` |
| Claude Code terminal excerpts | 2.1.316 |
| HyperFrames | 0.8.226 |
| Voice runtime | Apple Silicon, Python 3.12, MLX Audio 0.5.2 |
| VoiceDesign model | `mlx-community/Qwen3-TTS-12Hz-1.7B-VoiceDesign-bf16` |
| VoiceDesign revision | `7d3824abff87e49756bb0f83fb5411de75d160c4` |
| Base model | `mlx-community/Qwen3-TTS-12Hz-1.7B-Base-bf16` |
| Base revision | `a6eb4f68e4b056f1215157bb696209bc82a6db48` |

## Delivery specification

| Property | Final value |
| --- | --- |
| Canvas | 1080 × 1920, portrait 9:16 |
| Video | H.264, `yuv420p`, 30 fps, 4,909 frames |
| Duration | 163.633 seconds, approximately 2 minutes 44 seconds |
| MP4 size | 8,112,526 bytes |
| Audio | AAC stereo, 48 kHz; original synthetic Mandarin narrator |
| Captions | 31 cues, burned into the video; separate SRT retained |
| Measured loudness | -16.0 LUFS integrated; -4.4 dBFS true peak |
| Music | None |
| Cover source | `qwen-poster.png`, extracted at 2 seconds, 1080 × 1920 |
| Platform cover | 3:4 crop, repositioned to retain the top heading and lower switch instruction |

The initial duration estimate was shorter. Browser login, browser completion,
terminal success, and Enter were retained to make the whole login sequence
understandable. The final duration is the delivered value above.

## Visual specification

| Element | Value in canvas pixels |
| --- | --- |
| Palette | Background `#edf1ed`, ink `#1f2925`, accent `#aa4c2f` |
| Main console | x 72, y 350, width 936, height 1080; background `#141414` |
| Heading | x 80, y 168, width 920; Source Han Sans CN, 66 px |
| ccsw UI | 32-column terminal, Geist Mono, 44 px |
| Shell commands | 78–112 px |
| Field focus | Outline `#d7a67f`, explanation inside the console |
| Captions | 42 px, y 1540–1710 |
| Main content area | x 80–1000, y 168–1490; console shell extends to x 72–1008 |
| Browser panel | Extends to y 1490; terminal panels end at y 1430 |
| Motion targets | 12-frame entrances with `power2.out`; 18-frame state changes with `power2.inOut`; text holds still for reading |

The console occupies most of the frame. Native Claude Code welcome and prompt
excerpts replace the earlier generic terminal appearance. The browser flow
shows actual login and completion-page controls. The central cover crop
initially removed the heading; moving the image down in the 3:4 crop restored
the heading while retaining the account-switch instruction.

## Final scene timing

| Time in seconds | Scene | Required result |
| --- | --- | --- |
| 0.00–13.58 | Introduction | Show the account switch and its result |
| 13.58–28.03 | Save the first account | Terminal A is signed in; terminal B runs `ccsw add claude` |
| 28.03–74.03 | Add the second account | Complete the browser and terminal login sequence, then save |
| 74.03–93.03 | Switch accounts | Open ccsw, select Switch account, choose a row, press Enter; show `ccsw switch 1` |
| 93.03–123.18 | Main fields | Slot, organization, active account, used quota, resets, and model quota |
| 123.18–148.45 | Optional fields | Pace, reset grants and expiry, and extra usage |
| 148.45–163.62 | Closing | Brief `ccsw add codex` example and reference to the note |

The login sequence has these explicit transitions:

| Time in seconds | Visible state |
| --- | --- |
| 28.03 | Type `/login` inside the native Claude Code prompt |
| 33.03 | Open the actual browser login page |
| 40.03 | Enlarge the email form and illustrate continuing with an example address |
| 50.03 | Show the official browser completion page and explain returning to Claude Code |
| 56.03 | Show `Login successful. Press Enter to continue…` for 6.4 seconds |
| 62.43 | Press Enter and return to the native prompt |
| 64.03 | Move to terminal B and save with `ccsw add claude` |

The final [timeline](../tutorials/2026-11-03-claude-code/timeline-data.json)
preserves full cue timing, spoken text, and scene boundaries. The
[SRT](../tutorials/2026-11-03-claude-code/captions.zh-CN.srt) preserves the final
caption text and millisecond timing.

## Product rules and source fidelity

`/login` runs inside Claude Code. `ccsw add claude` runs in a shell after login
completes. The process repeats for each additional account, without first
running `/logout`. The dashboard's Add new account entry is a Codex flow in
the recorded ccsw version.

Account percentages mean **used** quota. Claude's bracketed label is the
organization or `personal`. `resets` is the quota reset countdown, a model row
has its own quota, and `ahead of pace` compares weekly consumption with a
uniform rate. The heart shows available resets and the earliest expiry;
`$$` shows extra-usage spend and the monthly limit. Data is cached. A running
Claude Code session backed by macOS Keychain can take about 30 seconds to pick
up a switched login. See the [CLI guide](../usage.md) for the full behavior.

ccsw frames use its actual Ratatui components with example.com accounts.
Claude Code excerpts came from an isolated CLI configuration with a dummy API
key. The API billing label was omitted because the lesson concerns saved
account logins; example identity labels sit outside the native UI.

Browser excerpts came from the official login page. The example email was
entered but not submitted. The public
[completion page](https://platform.claude.com/oauth/code/success?app=claude-code)
was opened directly. The terminal success panel is a labeled reconstruction
from verified CLI text and the
[authentication documentation](https://code.claude.com/docs/en/authentication).
The video does not record verification, consent, or a completed real login.

## Validation and publication

HyperFrames reported `ok: true` with browser checks enabled. Runtime, layout,
motion, contrast, and audio checks had no errors or warnings. Two lint warnings
concerned field-track density. Three reading warnings concerned intermediate
typing frames; the completed command remained visible for several seconds.

The final scene, phone-size, field-focus, and boundary sheets were reviewed.
The login completion sequence was inspected at 51, 58, and 63 seconds.
All 31 narration cues matched the local ASR transcript after normalization,
and each speech segment fit its caption slot. Full FFmpeg decode passed.
The narration was not reviewed by ear; ASR and level checks do not replace
that review.

The platform upload preview was checked before publication. The upload was
recognized as HD, and the cover assessment passed after the crop adjustment.
These observations supersede the pre-publication status in the ZIP's
`REVIEW.md`.

| Publication field | Recorded result |
| --- | --- |
| Account | Jacky Developer |
| Title | Claude Code多帐号切换教程 |
| Required topics | #AI替我说 #用AI讲知识 |
| Other topics | #ClaudeCode #AI编程 #开发工具 #编程效率 #ccsw |
| Content declaration | AI-generated content |
| Visibility | Public; scheduling off |
| Activity | AI替我说, associated by the selected topic |
| Submit result | Publish successful |
| Note-manager status | Under review |
| Platform timestamp | 2026-11-03 16:27, as displayed; no time-zone offset was shown |

The [post copy](../tutorials/2026-11-03-claude-code/post.zh-CN.txt) retains the
title, body, required topics, and a prepared pinned comment. Only the title and
body were posted. The pinned comment was not posted.

## Archive and recovery

The original local delivery root is
`~/Downloads/ccsw-xhs-claude-j6gj49hl/`. It contains:

| Relative path | Contents |
| --- | --- |
| `animation/` | Working HyperFrames project, scripts, source records, and checks |
| `animation/README.md`, `SPEC.md`, `STORYBOARD.md`, `REVIEW.md` | Original project instructions and pre-publication records, all under `animation/` |
| `animation/renders/ccsw-claude-qwen-preview.mp4` | Published video |
| `animation/renders/qwen-poster.png` | Uploaded cover source before platform crop |
| `ccsw-claude-qwen-hyperframes.zip` | Editable project, licensed assets, audio, and rendered video |
| `xiaohongshu-publication.json`, `xiaohongshu-published.jpg` | Publication receipt and screenshot |
| `revisions/` | Earlier versions |
| `tailscale-preview/serve.py` | Local preview server |

The ZIP is 29,929,502 bytes and has 85 entries under
`ccsw-claude-tutorial/`. It includes the assets required to rebuild the scenes,
so the original Rust fixture generator and raw CLI capture directory are not
required. Qwen model weights and the local ASR service are separate
prerequisites for regenerating voice and transcription checks.

SHA-256 checksums of the delivered files:

```text
b119991d6ec1e3decaf706f625f9b2d00040b59f9666afcd51d06ac63bb86a92  ccsw-claude-qwen-preview.mp4
d446c9c64c1bc37e5b25cd2db6e842e87846aa6a42f2fff25c174c071c411f11  qwen-poster.png
ab9ec605b18787f88dde6fc84609f1164974b4454b6a085baab19ab68253f067  ccsw-claude-qwen-hyperframes.zip
```

The recorded preview used the host's Tailscale address on port 8799, with media
version 5. It depends on that host and the local server. The media archive is
still local; no durable remote asset location has been recorded. Preserve the
ZIP with the checksums when moving it to long-term storage. This repository
retains the workflow, specification, timeline, captions, and post copy; it
does not by itself restore the media assets.
