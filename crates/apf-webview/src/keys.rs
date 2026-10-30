//! Keyboard shortcuts the page claims (`apf.keys.claim([...])`).
//!
//! Hosts run their own key commands before a plugin's view sees the key: Logic duplicates a
//! track on ⌘D, REAPER and Logic start playback on the space bar. A web view that uses such
//! a shortcut claims it; while the page has the keyboard focus a claimed key goes to the page
//! first, and the host gets it only when the page does not take it (`preventDefault`).
//! Shortcuts the page does not claim stay with the host (⌘S saves the project).

/// One shortcut: modifiers and a key, parsed from `"cmd+shift+z"`, `"backspace"`, `"space"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyCombo {
    pub cmd: bool,
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub key: Key,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A printable key, lowercase (`'d'`, `'1'`, `'='`).
    Char(char),
    Backspace,
    Delete,
    Escape,
    Space,
    Enter,
    Tab,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
}

impl KeyCombo {
    /// Parses `"cmd+d"`, `"Cmd+Shift+Z"`, `"alt+left"`, `"backspace"`; `meta`/`command` mean
    /// cmd, `option` means alt, `control` means ctrl. `None` for anything else.
    pub fn parse(text: &str) -> Option<KeyCombo> {
        let mut combo = KeyCombo { cmd: false, shift: false, alt: false, ctrl: false, key: Key::Space };
        let mut key = None;
        let lower = text.trim().to_ascii_lowercase();
        // "cmd++" is the plus key.
        let parts: Vec<&str> = if let Some(head) = lower.strip_suffix("++") {
            head.split('+').chain(std::iter::once("+")).collect()
        } else {
            lower.split('+').collect()
        };
        for part in parts {
            match part.trim() {
                "cmd" | "command" | "meta" => combo.cmd = true,
                "shift" => combo.shift = true,
                "alt" | "option" | "opt" => combo.alt = true,
                "ctrl" | "control" => combo.ctrl = true,
                name if key.is_none() => key = Some(Self::key_named(name)?),
                _ => return None,
            }
        }
        combo.key = key?;
        Some(combo)
    }

    fn key_named(name: &str) -> Option<Key> {
        Some(match name {
            "backspace" => Key::Backspace,
            "delete" | "del" => Key::Delete,
            "escape" | "esc" => Key::Escape,
            "space" | " " => Key::Space,
            "enter" | "return" => Key::Enter,
            "tab" => Key::Tab,
            "left" | "arrowleft" => Key::Left,
            "right" | "arrowright" => Key::Right,
            "up" | "arrowup" => Key::Up,
            "down" | "arrowdown" => Key::Down,
            "home" => Key::Home,
            "end" => Key::End,
            "pageup" => Key::PageUp,
            "pagedown" => Key::PageDown,
            s => {
                let mut chars = s.chars();
                let c = chars.next()?;
                if chars.next().is_some() {
                    return None;
                }
                Key::Char(c)
            }
        })
    }
}

/// What the page claimed, from its `"keys ..."` message.
#[derive(Debug, Default, Clone)]
pub struct Claims(Vec<KeyCombo>);

impl Claims {
    /// `"keys cmd+d cmd+z backspace"` (space separated; an empty list claims nothing).
    pub fn parse(list: &str) -> Claims {
        Claims(list.split_ascii_whitespace().filter_map(KeyCombo::parse).collect())
    }

    pub fn contains(&self, combo: &KeyCombo) -> bool {
        self.0.iter().any(|c| c == combo || (c.key == combo.key && Self::same_mods_but_shift_char(c, combo)))
    }

    /// `"cmd+="` also matches ⌘+ on a keyboard where + needs shift: for printable keys shift
    /// is part of the character, so a claim without shift matches either.
    fn same_mods_but_shift_char(claim: &KeyCombo, pressed: &KeyCombo) -> bool {
        matches!(claim.key, Key::Char(_))
            && !claim.shift
            && claim.cmd == pressed.cmd
            && claim.alt == pressed.alt
            && claim.ctrl == pressed.ctrl
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_parse_and_match() {
        let c = KeyCombo::parse("Cmd+Shift+Z").unwrap();
        assert_eq!((c.cmd, c.shift, c.alt, c.ctrl, c.key), (true, true, false, false, Key::Char('z')));
        assert_eq!(KeyCombo::parse("backspace").unwrap().key, Key::Backspace);
        assert_eq!(KeyCombo::parse("meta+d"), KeyCombo::parse("cmd+d"));
        assert_eq!(KeyCombo::parse("cmd++").unwrap().key, Key::Char('+'));
        assert!(KeyCombo::parse("cmd+").is_none());
        assert!(KeyCombo::parse("cmd+dd").is_none());
        assert!(KeyCombo::parse("cmd+d+e").is_none());

        let claims = Claims::parse("cmd+d cmd+z cmd+shift+z backspace nonsense+x cmd+=");
        assert!(claims.contains(&KeyCombo::parse("cmd+d").unwrap()));
        assert!(claims.contains(&KeyCombo::parse("cmd+shift+z").unwrap()));
        assert!(!claims.contains(&KeyCombo::parse("cmd+s").unwrap()), "not claimed: stays with the host");
        assert!(!claims.contains(&KeyCombo::parse("space").unwrap()));
        assert!(!claims.contains(&KeyCombo::parse("cmd+alt+d").unwrap()), "other modifiers are another shortcut");
        assert!(claims.contains(&KeyCombo::parse("cmd+shift+=").unwrap()), "shift belongs to the character");
        assert!(Claims::parse("").is_empty());
    }
}
