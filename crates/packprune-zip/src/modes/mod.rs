//! Zip layouts: what each mode changes in the records it writes.

use crate::ZipMode;

impl ZipMode {
    /// The name `zip.mode` takes in the config.
    pub fn name(self) -> &'static str {
        match self {
            Self::Conformant => "conformant",
            Self::Lean => "lean",
        }
    }

    /// Identical (method, CRC, size, payload) entries point at one local record.
    pub(crate) fn shares_data(self) -> bool {
        self == Self::Lean
    }

    /// The client's `java.util.zip.ZipFile` takes names from the central directory and only skips the local name, so
    /// `lean` leaves it empty, along with the UTF-8 flag that would describe it.
    pub(crate) fn local_name(self, name: &str, flags: u16) -> (&[u8], u16) {
        match self {
            Self::Lean => (&[], 0),
            Self::Conformant => (name.as_bytes(), flags),
        }
    }
}
