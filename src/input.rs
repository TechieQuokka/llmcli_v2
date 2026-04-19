/// Minimal line editor that handles:
///   - UTF-8 multibyte characters (Korean 3-byte UTF-8, CJK, etc.)
///   - Backspace / Delete over multibyte chars (char-boundary aware)
///   - Left / Right arrow key cursor movement
///   - Ctrl-C / Ctrl-D signals
///   - No external TUI dependency — uses only termios via libc syscalls
///
/// Returns Ok(None) on EOF (Ctrl-D on empty line).
/// Returns Err on Ctrl-C so the caller can handle SIGINT.


use std::io::{self, Write};

#[derive(Debug)]
pub enum LineResult {
    Line(String),
    Eof,        // Ctrl-D on empty line
    Interrupted, // Ctrl-C
}

/// Read one byte from stdin via libc (bypasses Rust's BufReader, raw mode assumed).
fn read_byte() -> io::Result<u8> {
    unsafe {
        let mut b = [0u8; 1];
        let n = libc::read(libc::STDIN_FILENO, b.as_mut_ptr() as *mut libc::c_void, 1);
        if n == 1 { Ok(b[0]) } else { Err(io::Error::last_os_error()) }
    }
}

/// Peek at the next byte with a ~100 ms timeout. Used to distinguish bare ESC
/// from the start of an escape sequence (arrow keys, etc.).
#[cfg(unix)]
fn read_byte_timeout() -> Option<u8> {
    unsafe {
        let fd = libc::STDIN_FILENO;
        let mut fds = std::mem::zeroed::<libc::fd_set>();
        libc::FD_SET(fd, &mut fds);
        let mut tv = libc::timeval { tv_sec: 0, tv_usec: 100_000 };
        let ready = libc::select(fd + 1, &mut fds, std::ptr::null_mut(), std::ptr::null_mut(), &mut tv);
        if ready <= 0 { return None; }
        let mut b = [0u8; 1];
        let n = libc::read(fd, b.as_mut_ptr() as *mut libc::c_void, 1);
        if n == 1 { Some(b[0]) } else { None }
    }
}

/// Watches stdin for ESC during streaming. Dropped when streaming ends.
///
/// Uses a self-pipe so the background thread can be woken up instantly
/// when streaming finishes, without polling.
#[cfg(unix)]
pub struct EscMonitor {
    handle: Option<std::thread::JoinHandle<()>>,
    stop_write_fd: libc::c_int,
}

#[cfg(unix)]
impl EscMonitor {
    pub fn start(interrupted: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Self {
        let mut pipe_fds = [0i32; 2];
        unsafe { libc::pipe(pipe_fds.as_mut_ptr()); }
        let (stop_read_fd, stop_write_fd) = (pipe_fds[0], pipe_fds[1]);

        let handle = std::thread::spawn(move || {
            // input-only raw: disables echo/canon but keeps OPOST so streamed
            // output \n continues to work as \r\n on screen.
            let old = unsafe { raw::enable_input_only(libc::STDIN_FILENO) };
            let stdin = libc::STDIN_FILENO;
            let max_fd = stdin.max(stop_read_fd) + 1;
            unsafe {
                loop {
                    let mut fds = std::mem::zeroed::<libc::fd_set>();
                    libc::FD_SET(stdin, &mut fds);
                    libc::FD_SET(stop_read_fd, &mut fds);
                    let r = libc::select(max_fd, &mut fds, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
                    if r <= 0 { break; }
                    if libc::FD_ISSET(stop_read_fd, &fds) { break; }
                    if libc::FD_ISSET(stdin, &fds) {
                        let mut b = [0u8; 1];
                        let n = libc::read(stdin, b.as_mut_ptr() as *mut libc::c_void, 1);
                        if n == 1 && b[0] == 0x1B {
                            interrupted.store(true, std::sync::atomic::Ordering::SeqCst);
                            break;
                        }
                    }
                }
                libc::close(stop_read_fd);
                raw::disable(libc::STDIN_FILENO, &old);
            }
        });

        Self { handle: Some(handle), stop_write_fd }
    }
}

#[cfg(unix)]
impl Drop for EscMonitor {
    fn drop(&mut self) {
        unsafe {
            let b = [1u8];
            libc::write(self.stop_write_fd, b.as_ptr() as *const libc::c_void, 1);
            libc::close(self.stop_write_fd);
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Returns the byte-length of the UTF-8 sequence starting with `first`.
fn utf8_seq_len(first: u8) -> usize {
    if first < 0x80 { 1 }
    else if first < 0xE0 { 2 }
    else if first < 0xF0 { 3 }
    else { 4 }
}

/// Visual column width of a UTF-8 string (Korean / CJK = 2 columns each).
fn display_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

fn char_width(c: char) -> usize {
    // CJK Unified, Hangul, fullwidth forms, etc.
    match c as u32 {
        0x1100..=0x115F   // Hangul Jamo
        | 0x2E80..=0x303E // CJK radicals
        | 0x3040..=0x33FF // Hiragana / Katakana / CJK compat
        | 0x3400..=0x4DBF // CJK ext A
        | 0x4E00..=0x9FFF // CJK unified
        | 0xA000..=0xA4CF // Yi
        | 0xAC00..=0xD7AF // Hangul syllables
        | 0xF900..=0xFAFF // CJK compat ideographs
        | 0xFE10..=0xFE19 // Vertical forms
        | 0xFE30..=0xFE4F // CJK compat forms
        | 0xFF00..=0xFF60 // Fullwidth
        | 0xFFE0..=0xFFE6 => 2,
        _ => 1,
    }
}

/// A simple line-editing state machine.
struct LineEditor {
    /// Buffer as bytes (always valid UTF-8)
    buf: Vec<u8>,
    /// Cursor position in *bytes* within buf
    cursor: usize,
    /// The prompt string (for redraw)
    prompt: String,
    /// Number of terminal lines currently rendered below the prompt line
    display_lines: usize,
}

impl LineEditor {
    fn new(prompt: &str) -> Self {
        Self { buf: Vec::new(), cursor: 0, prompt: prompt.to_owned(), display_lines: 0 }
    }

    fn as_str(&self) -> &str {
        // SAFETY: we only ever insert valid UTF-8
        unsafe { std::str::from_utf8_unchecked(&self.buf) }
    }

    /// Insert bytes at cursor position (must be valid UTF-8 sequence).
    fn insert(&mut self, bytes: &[u8]) {
        for (i, &b) in bytes.iter().enumerate() {
            self.buf.insert(self.cursor + i, b);
        }
        self.cursor += bytes.len();
    }

    /// Delete the character immediately before the cursor (backspace).
    fn backspace(&mut self) -> bool {
        if self.cursor == 0 { return false; }
        // Walk back to find char boundary
        let mut start = self.cursor - 1;
        while start > 0 && (self.buf[start] & 0xC0) == 0x80 {
            start -= 1;
        }
        let _ch_bytes = self.cursor - start;
        self.buf.drain(start..self.cursor);
        self.cursor = start;
        // Erase the visual columns occupied by that character
        let ch = std::str::from_utf8(&self.buf[self.cursor..]).ok()
            .and_then(|s| s.chars().next());
        let _ = ch; // for now just erase 2 cols max
        true
    }

    /// Move cursor one character left.
    fn move_left(&mut self) -> bool {
        if self.cursor == 0 { return false; }
        let mut pos = self.cursor - 1;
        while pos > 0 && (self.buf[pos] & 0xC0) == 0x80 {
            pos -= 1;
        }
        self.cursor = pos;
        true
    }

    /// Move cursor one character right.
    fn move_right(&mut self) -> bool {
        if self.cursor >= self.buf.len() { return false; }
        let first = self.buf[self.cursor];
        self.cursor += utf8_seq_len(first);
        true
    }

    /// Redraw entire input area from prompt (supports multiline buffers).
    fn redraw(&mut self) {
        let mut out = io::stdout();
        let content = unsafe { std::str::from_utf8_unchecked(&self.buf) };
        let new_lines = content.chars().filter(|&c| c == '\n').count();

        // Move back up by however many lines are currently rendered on screen.
        if self.display_lines > 0 {
            let _ = write!(out, "\x1b[{}A", self.display_lines);
        }

        // Return to column 0, clear from cursor to end of screen
        let _ = out.write_all(b"\r\x1b[J");
        let _ = out.write_all(self.prompt.as_bytes());

        // Print buffer, converting \n → \r\n for raw mode
        for ch in content.chars() {
            if ch == '\n' {
                let _ = out.write_all(b"\r\n");
            } else {
                let mut tmp = [0u8; 4];
                let s = ch.encode_utf8(&mut tmp);
                let _ = out.write_all(s.as_bytes());
            }
        }

        self.display_lines = new_lines;

        // Reposition cursor: count display columns from cursor to end
        let tail = unsafe { std::str::from_utf8_unchecked(&self.buf[self.cursor..]) };
        let newlines_in_tail = tail.chars().filter(|&c| c == '\n').count();
        if newlines_in_tail > 0 {
            let _ = write!(out, "\x1b[{}A", newlines_in_tail);
        }
        let last_line_tail = tail.rsplit('\n').next().unwrap_or(tail);
        let cols_back = display_width(last_line_tail);
        if cols_back > 0 {
            let _ = write!(out, "\x1b[{}D", cols_back);
        }
        let _ = out.flush();
    }
}

// ── Raw mode helpers (POSIX only) ─────────────────────────────────────────────

#[cfg(unix)]
mod raw {
    use std::os::unix::io::RawFd;

    pub fn enable(fd: RawFd) -> libc::termios {
        let mut old = unsafe { std::mem::zeroed::<libc::termios>() };
        unsafe { libc::tcgetattr(fd, &mut old) };
        let mut raw = old;
        // cfmakeraw equivalent
        raw.c_iflag &= !(libc::IGNBRK | libc::BRKINT | libc::PARMRK
            | libc::ISTRIP | libc::INLCR | libc::IGNCR
            | libc::ICRNL | libc::IXON);
        raw.c_oflag &= !libc::OPOST;
        raw.c_lflag &= !(libc::ECHO | libc::ECHONL | libc::ICANON
            | libc::ISIG | libc::IEXTEN);
        raw.c_cflag &= !(libc::CSIZE | libc::PARENB);
        raw.c_cflag |= libc::CS8;
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &raw) };
        old
    }

    /// Like enable() but preserves OPOST so \n→\r\n output processing keeps working.
    /// Used by EscMonitor which only needs to suppress echo/canon for input.
    pub fn enable_input_only(fd: RawFd) -> libc::termios {
        let mut old = unsafe { std::mem::zeroed::<libc::termios>() };
        unsafe { libc::tcgetattr(fd, &mut old) };
        let mut raw = old;
        raw.c_iflag &= !(libc::IGNBRK | libc::BRKINT | libc::PARMRK
            | libc::ISTRIP | libc::INLCR | libc::IGNCR
            | libc::ICRNL | libc::IXON);
        // c_oflag intentionally NOT touched — keep OPOST so \n stays \r\n
        raw.c_lflag &= !(libc::ECHO | libc::ECHONL | libc::ICANON
            | libc::ISIG | libc::IEXTEN);
        raw.c_cflag &= !(libc::CSIZE | libc::PARENB);
        raw.c_cflag |= libc::CS8;
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &raw) };
        old
    }

    pub fn disable(fd: RawFd, old: &libc::termios) {
        unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, old) };
    }
}

pub struct RawGuard {
    #[cfg(unix)]
    old: libc::termios,
}

impl RawGuard {
    pub fn enable() -> Self {
        #[cfg(unix)]
        {
            let old = raw::enable(libc::STDIN_FILENO);
            Self { old }
        }
        #[cfg(not(unix))]
        Self {}
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        raw::disable(libc::STDIN_FILENO, &self.old);
    }
}

/// Read one full logical line from the terminal using a minimal line editor.
/// `prompt` is printed before accepting input.
pub fn read_line(prompt: &str) -> LineResult {
    let mut out = io::stdout();
    let _ = out.write_all(prompt.as_bytes());
    let _ = out.flush();

    // _guard is dropped exactly once when this fn returns,
    // restoring the terminal regardless of exit path.
    let _guard = RawGuard::enable();
    // Enable bracketed paste mode; restored on drop.
    let _ = out.write_all(b"\x1b[?2004h");
    let _ = out.flush();
    struct BpGuard;
    impl Drop for BpGuard {
        fn drop(&mut self) {
            let _ = io::stdout().write_all(b"\x1b[?2004l");
            let _ = io::stdout().flush();
        }
    }
    let _bp = BpGuard;
    let mut ed = LineEditor::new(prompt);

    loop {
        // Read first byte (may be escape, control, or start of UTF-8)
        let b = match read_byte() {
            Ok(b) => b,
            Err(_) => {
                let _ = out.write_all(b"\r\n");
                let _ = out.flush();
                return LineResult::Eof;
            }
        };

        match b {
            // Enter / newline
            b'\r' | b'\n' => {
                // '\' immediately before cursor → line continuation
                if ed.buf.last() == Some(&b'\\') {
                    ed.buf.pop();
                    ed.cursor = ed.cursor.saturating_sub(1);
                    ed.insert(b"\n");
                    ed.redraw();
                } else {
                    let _ = out.write_all(b"\r\n");
                    let _ = out.flush();
                    return LineResult::Line(ed.as_str().to_owned());
                }
            }

            // Ctrl-C
            0x03 => {
                let _ = out.write_all(b"\r\n");
                let _ = out.flush();
                return LineResult::Interrupted;
            }

            // Ctrl-D (EOF on empty line)
            0x04 => {
                if ed.buf.is_empty() {
                    let _ = out.write_all(b"\r\n");
                    let _ = out.flush();
                    return LineResult::Eof;
                }
                // Ctrl-D on non-empty → ignore
                continue;
            }

            // Backspace (0x7F DEL or 0x08)
            0x7F | 0x08 => {
                if ed.backspace() {
                    ed.redraw();
                }
            }

            // ESC sequence (arrow keys, etc.)
            0x1B => {
                #[cfg(unix)]
                let b2 = read_byte_timeout().unwrap_or(0);
                #[cfg(not(unix))]
                let b2 = read_byte().unwrap_or(0);

                if b2 == b'[' {
                    let b3 = read_byte().unwrap_or(0);
                    match b3 {
                        b'D' => { // left arrow
                            if ed.move_left() { ed.redraw(); }
                        }
                        b'C' => { // right arrow
                            if ed.move_right() { ed.redraw(); }
                        }
                        b'A' | b'B' => {} // up/down: ignore history for now
                        b'3' => {
                            let b4 = read_byte().unwrap_or(0);
                            if b4 == b'~' { // Delete key
                                if ed.cursor < ed.buf.len() {
                                    let first = ed.buf[ed.cursor];
                                    let len = utf8_seq_len(first);
                                    ed.buf.drain(ed.cursor..ed.cursor + len);
                                    ed.redraw();
                                }
                            }
                        }
                        b'2' => {
                            // Read rest of numeric sequence until '~'
                            let mut seq = vec![b'2'];
                            loop {
                                let nx = read_byte().unwrap_or(0);
                                seq.push(nx);
                                if nx == b'~' || nx == 0 { break; }
                            }
                            if seq == b"200~" {
                                // ESC[200~ — bracketed paste start: collect until ESC[201~
                                let mut paste: Vec<u8> = Vec::new();
                                'paste: loop {
                                    let pb = match read_byte() {
                                        Ok(b) => b,
                                        Err(_) => break 'paste,
                                    };
                                    if pb == 0x1B {
                                        let pb2 = read_byte().unwrap_or(0);
                                        if pb2 == b'[' {
                                            let mut end = Vec::new();
                                            loop {
                                                let nx = read_byte().unwrap_or(0);
                                                end.push(nx);
                                                if nx == b'~' || nx == 0 { break; }
                                            }
                                            if end == b"201~" {
                                                break 'paste;
                                            }
                                            // Not the end marker — keep the bytes
                                            paste.push(pb);
                                            paste.push(pb2);
                                            paste.extend_from_slice(&end);
                                        } else {
                                            paste.push(pb);
                                            paste.push(pb2);
                                        }
                                    } else if pb == b'\r' {
                                        paste.push(b'\n');
                                    } else {
                                        paste.push(pb);
                                    }
                                }
                                if std::str::from_utf8(&paste).is_ok() {
                                    ed.insert(&paste);
                                    ed.redraw();
                                }
                            }
                            // other numeric ESC sequences — already consumed
                        }
                        b'3'..=b'9' => {
                            // consume unknown numeric ESC sequence until ~
                            loop {
                                let nx = read_byte().unwrap_or(0);
                                if nx == b'~' || nx == 0 { break; }
                            }
                        }
                        _ => {}
                    }
                }
            }

            // Regular printable byte (ASCII or UTF-8 lead byte)
            b if b >= 0x20 || (b >= 0x80) => {
                let extra = utf8_seq_len(b).saturating_sub(1);
                let mut bytes = vec![b];
                for _ in 0..extra {
                    match read_byte() {
                        Ok(cont) => bytes.push(cont),
                        Err(_) => break,
                    }
                }
                // Validate UTF-8 before inserting
                if std::str::from_utf8(&bytes).is_ok() {
                    // Echo the character(s) to terminal
                    let _ = out.write_all(&bytes);
                    let _ = out.flush();
                    ed.insert(&bytes);
                    // If cursor is not at end, redraw to reposition
                    if ed.cursor < ed.buf.len() {
                        ed.redraw();
                    }
                }
            }

            _ => {} // ignore other control bytes
        }
    }
}
