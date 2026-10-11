//! Terminal graphics detection before the event loop takes ownership of input.

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use ratatui_image::FontSize;
use ratatui_image::picker::cap_parser::{Parser, QueryStdioOptions, Response};
use ratatui_image::picker::{Picker, ProtocolType};

pub(super) fn detect_picker() -> Option<Picker> {
    let mut terminal = terminal_trx::terminal().ok()?;
    let mut terminal = terminal.lock();
    let mut terminal = terminal.enable_raw_mode().ok()?;
    let is_tmux = Picker::halfblocks().tmux_detected();
    let responses = query(
        &mut terminal,
        is_tmux,
        Duration::from_millis(500),
        wait_readable,
    )
    .ok()?;
    let font = responses
        .iter()
        .find_map(|response| match response {
            Response::CellSize(Some((width, height))) => Some(FontSize::new(*width, *height)),
            _ => None,
        })
        .or_else(window_font_size)?;
    // The library's query leaves a blocked input thread behind on timeout. Poll on this thread
    // instead, then use its environment and tmux handling with the measured cell size.
    #[allow(deprecated)]
    let mut picker = Picker::from_fontsize(font);
    let blocked = ["WEZTERM_EXECUTABLE", "KONSOLE_VERSION"]
        .iter()
        .any(|name| std::env::var(name).is_ok_and(|value| !value.is_empty()));
    let protocol = select_protocol(&responses, picker.protocol_type(), blocked);
    if protocol == ProtocolType::Halfblocks {
        return None;
    }
    picker.set_protocol_type(protocol);
    Some(picker)
}

fn window_font_size() -> Option<FontSize> {
    let size = crossterm::terminal::window_size().ok()?;
    let width = size.width.checked_div(size.columns)?;
    let height = size.height.checked_div(size.rows)?;
    (width > 0 && height > 0).then_some(FontSize::new(width, height))
}

fn select_protocol(responses: &[Response], fallback: ProtocolType, blocked: bool) -> ProtocolType {
    if !blocked && responses.contains(&Response::Kitty) {
        ProtocolType::Kitty
    } else if !blocked && responses.contains(&Response::Sixel) {
        ProtocolType::Sixel
    } else {
        fallback
    }
}

fn query<T: Read + Write>(
    terminal: &mut T,
    is_tmux: bool,
    timeout: Duration,
    mut wait: impl FnMut(&T, Duration) -> io::Result<bool>,
) -> io::Result<Vec<Response>> {
    terminal.write_all(Parser::query(is_tmux, QueryStdioOptions::default()).as_bytes())?;
    terminal.flush()?;
    let deadline = Instant::now() + timeout;
    let mut parser = Parser::new();
    let mut responses = Vec::new();
    // Read one byte at a time so keys after the final status report stay in the input queue.
    let mut byte = [0];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || !wait(terminal, remaining)? || terminal.read(&mut byte)? == 0 {
            break;
        }
        for response in parser.push(char::from(byte[0])) {
            if response == Response::Status {
                return Ok(responses);
            }
            responses.push(response);
        }
    }
    Ok(responses)
}

#[cfg(unix)]
fn wait_readable(terminal: &impl terminal_trx::Transceive, timeout: Duration) -> io::Result<bool> {
    let mut fd = libc::pollfd {
        fd: terminal.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: The borrowed terminal descriptor and the single pollfd stay valid for this call.
    let result =
        unsafe { libc::poll(&mut fd, 1, timeout.as_millis().min(i32::MAX as u128) as i32) };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result > 0 && fd.revents & libc::POLLIN != 0)
    }
}

#[cfg(windows)]
fn wait_readable(terminal: &impl terminal_trx::Transceive, timeout: Duration) -> io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::WaitForSingleObject;

    let handle = terminal.input_buffer_handle();
    // SAFETY: The console handle is borrowed for the duration of this call.
    match unsafe { WaitForSingleObject(handle.as_raw_handle(), timeout.as_millis() as u32) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        _ => Err(io::Error::last_os_error()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct Terminal {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl Read for Terminal {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            self.input.read(bytes)
        }
    }

    impl Write for Terminal {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.output.write(bytes)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn capability_reply_preserves_keys_after_the_status_report() {
        let mut terminal = Terminal {
            input: Cursor::new(b"\x1b_Gi=31;OK\x1b\\\x1b[?64;4c\x1b[6;20;10t\x1b[0nq".to_vec()),
            output: Vec::new(),
        };
        let responses = query(&mut terminal, false, Duration::from_secs(1), |_, _| {
            Ok(true)
        })
        .unwrap();
        assert_eq!(
            responses,
            [
                Response::Kitty,
                Response::Sixel,
                Response::CellSize(Some((10, 20)))
            ]
        );
        let mut key = String::new();
        terminal.read_to_string(&mut key).unwrap();
        assert_eq!(key, "q");
        assert_eq!(
            select_protocol(&responses, ProtocolType::Halfblocks, false),
            ProtocolType::Kitty
        );
        assert_eq!(
            select_protocol(&responses, ProtocolType::Iterm2, true),
            ProtocolType::Iterm2
        );
        assert_eq!(
            select_protocol(&[Response::Sixel], ProtocolType::Halfblocks, false),
            ProtocolType::Sixel
        );
    }

    #[test]
    fn unsupported_terminal_returns_without_reading_input() {
        let mut terminal = Terminal {
            input: Cursor::new(b"q".to_vec()),
            output: Vec::new(),
        };
        let responses = query(&mut terminal, false, Duration::from_millis(500), |_, _| {
            Ok(false)
        })
        .unwrap();
        assert!(responses.is_empty());
        assert_eq!(terminal.input.position(), 0);
        assert_eq!(
            select_protocol(&responses, ProtocolType::Halfblocks, false),
            ProtocolType::Halfblocks
        );
    }
}
