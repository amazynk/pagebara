//! Field flags (`/Ff`, ISO 32000-2 tables 227, 229, 231 and 233).

use std::fmt;

/// The `/Ff` bits of a field. Bit `n` of the standard (1-based) is `1 << (n - 1)`.
#[derive(Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct FieldFlags(pub u32);

macro_rules! flags {
    ($($(#[$m:meta])* $name:ident = $bit:expr;)*) => {
        impl FieldFlags {
            $($(#[$m])* pub const $name: FieldFlags = FieldFlags(1 << ($bit - 1));)*

            const NAMES: &[(&str, u32)] = &[$((stringify!($name), 1 << ($bit - 1))),*];
        }
    };
}

flags! {
    READ_ONLY = 1;
    REQUIRED = 2;
    NO_EXPORT = 3;
    // Text fields.
    MULTILINE = 13;
    PASSWORD = 14;
    FILE_SELECT = 21;
    DO_NOT_SPELL_CHECK = 23;
    DO_NOT_SCROLL = 24;
    COMB = 25;
    RICH_TEXT = 26;
    // Buttons.
    NO_TOGGLE_TO_OFF = 15;
    RADIO = 16;
    PUSHBUTTON = 17;
    RADIOS_IN_UNISON = 26;
    // Choice fields.
    COMBO = 18;
    EDIT = 19;
    SORT = 20;
    MULTI_SELECT = 22;
    COMMIT_ON_SEL_CHANGE = 27;
}

impl FieldFlags {
    pub fn contains(self, f: FieldFlags) -> bool {
        self.0 & f.0 == f.0
    }

    pub fn with(self, f: FieldFlags, on: bool) -> FieldFlags {
        if on {
            FieldFlags(self.0 | f.0)
        } else {
            FieldFlags(self.0 & !f.0)
        }
    }

    pub fn read_only(self) -> bool {
        self.contains(Self::READ_ONLY)
    }

    pub fn required(self) -> bool {
        self.contains(Self::REQUIRED)
    }

    pub fn no_export(self) -> bool {
        self.contains(Self::NO_EXPORT)
    }
}

impl std::ops::BitOr for FieldFlags {
    type Output = FieldFlags;

    fn bitor(self, o: FieldFlags) -> FieldFlags {
        FieldFlags(self.0 | o.0)
    }
}

impl fmt::Debug for FieldFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Bits shared between field types (26) are listed under each name.
        let names: Vec<&str> = Self::NAMES
            .iter()
            .filter(|(_, b)| self.0 & b != 0)
            .map(|(n, _)| *n)
            .collect();
        write!(f, "FieldFlags({:#x}: {})", self.0, names.join("|"))
    }
}
