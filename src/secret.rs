//! The password buffer: locked in memory, zeroed on drop, never printable.

use zeroize::Zeroize as _;

/// Bytes reserved up front so typing never reallocates (a reallocation would leave a copy of the
/// password in freed memory). Longer input is refused rather than reallocated.
const CAPACITY: usize = 1024;

pub struct SecretBuf {
    buf: Box<[u8; CAPACITY]>,
    len: usize,
    locked: bool,
}

impl SecretBuf {
    pub fn new() -> Self {
        let buf = Box::new([0u8; CAPACITY]);
        // SAFETY: the pointer and length describe the boxed array, which lives as long as `self`.
        let locked = unsafe { libc::mlock(buf.as_ptr().cast(), CAPACITY) } == 0;
        if !locked {
            tracing::warn!(
                "mlock failed; the password buffer may be swapped to disk: {}",
                std::io::Error::last_os_error()
            );
        }
        Self {
            buf,
            len: 0,
            locked,
        }
    }

    /// Append `s`. Returns `false` and leaves the buffer unchanged if it would not fit.
    pub fn push_str(&mut self, s: &str) -> bool {
        let bytes = s.as_bytes();
        if self.len + bytes.len() > CAPACITY {
            return false;
        }
        self.buf[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
        true
    }

    /// Remove the last UTF-8 character.
    pub fn pop_char(&mut self) {
        while self.len > 0 {
            self.len -= 1;
            let byte = self.buf[self.len];
            self.buf[self.len] = 0;
            // Stop after removing the lead byte of a character.
            if byte & 0xc0 != 0x80 {
                break;
            }
        }
    }

    pub fn clear(&mut self) {
        self.buf[..self.len].zeroize();
        self.len = 0;
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_str(&self) -> &str {
        // Only whole `&str`s are pushed and only whole characters popped.
        std::str::from_utf8(&self.buf[..self.len]).expect("buffer holds valid UTF-8")
    }

    /// Move the contents out into a new buffer and clear this one.
    pub fn take(&mut self) -> SecretBuf {
        let mut out = SecretBuf::new();
        out.push_str(self.as_str());
        self.clear();
        out
    }
}

impl Drop for SecretBuf {
    fn drop(&mut self) {
        self.buf.zeroize();
        if self.locked {
            // SAFETY: same region as the `mlock` in `new`.
            unsafe { libc::munlock(self.buf.as_ptr().cast(), CAPACITY) };
        }
    }
}

impl std::fmt::Debug for SecretBuf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretBuf(<{} bytes>)", self.len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pop_removes_whole_characters() {
        let mut s = SecretBuf::new();
        s.push_str("aé😀");
        s.pop_char();
        assert_eq!(s.as_str(), "aé");
        s.pop_char();
        assert_eq!(s.as_str(), "a");
        s.pop_char();
        s.pop_char();
        assert!(s.is_empty());
    }

    #[test]
    fn refuses_input_past_capacity() {
        let mut s = SecretBuf::new();
        assert!(s.push_str(&"x".repeat(CAPACITY)));
        assert!(!s.push_str("y"));
        assert_eq!(s.as_str().len(), CAPACITY);
    }

    #[test]
    fn take_moves_and_clears() {
        let mut s = SecretBuf::new();
        s.push_str("hunter2");
        let t = s.take();
        assert!(s.is_empty());
        assert_eq!(t.as_str(), "hunter2");
        assert_eq!(format!("{t:?}"), "SecretBuf(<7 bytes>)");
    }
}
