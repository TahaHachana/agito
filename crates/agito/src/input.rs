//! Mouse and keyboard input, dispatched as trusted browser events.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use agito_protocol::cdp::input;
use tokio::sync::Mutex;

use crate::error::{Error, Result};
use crate::transport::Session;

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseButton {
    /// The primary button.
    #[default]
    Left,
    /// The secondary button.
    Right,
    /// The wheel button.
    Middle,
}

impl MouseButton {
    fn cdp(self) -> input::MouseButton {
        match self {
            MouseButton::Left => input::MouseButton::Left,
            MouseButton::Right => input::MouseButton::Right,
            MouseButton::Middle => input::MouseButton::Middle,
        }
    }
}

/// Options for a click.
#[derive(Debug, Clone, Copy, Default)]
pub struct ClickOptions {
    /// Which button to press.
    pub button: MouseButton,
    /// How many clicks this is (2 for the second click of a double click).
    pub click_count: Option<u32>,
    /// Modifier keys held during the click, as a CDP bitmask (Alt=1, Control=2, Meta=4, Shift=8).
    pub modifiers: u32,
}

/// The page's mouse. Coordinates are CSS pixels relative to the viewport.
#[derive(Clone)]
pub struct Mouse {
    session: Arc<dyn Session>,
    position: Arc<Mutex<(f64, f64)>>,
}

impl Mouse {
    pub(crate) fn new(session: Arc<dyn Session>) -> Self {
        Self {
            session,
            position: Arc::new(Mutex::new((0.0, 0.0))),
        }
    }

    async fn dispatch(
        &self,
        kind: input::DispatchMouseEventParamsType,
        x: f64,
        y: f64,
        button: Option<MouseButton>,
        click_count: Option<u32>,
        modifiers: u32,
    ) -> Result<()> {
        self.session
            .send(&input::DispatchMouseEventParams {
                r#type: kind,
                x,
                y,
                modifiers: (modifiers != 0).then_some(modifiers as i64),
                button: button.map(MouseButton::cdp),
                click_count: click_count.map(i64::from),
                ..mouse_defaults()
            })
            .await?;
        Ok(())
    }

    /// Moves the pointer to `(x, y)` in `steps` intermediate moves.
    pub async fn move_to(&self, x: f64, y: f64, steps: u32) -> Result<()> {
        let mut position = self.position.lock().await;
        let (from_x, from_y) = *position;
        let steps = steps.max(1);
        for i in 1..=steps {
            let t = f64::from(i) / f64::from(steps);
            self.dispatch(
                input::DispatchMouseEventParamsType::MouseMoved,
                from_x + (x - from_x) * t,
                from_y + (y - from_y) * t,
                None,
                None,
                0,
            )
            .await?;
        }
        *position = (x, y);
        Ok(())
    }

    /// Presses a button at the current position.
    pub async fn down(&self, options: ClickOptions) -> Result<()> {
        let (x, y) = *self.position.lock().await;
        self.dispatch(
            input::DispatchMouseEventParamsType::MousePressed,
            x,
            y,
            Some(options.button),
            Some(options.click_count.unwrap_or(1)),
            options.modifiers,
        )
        .await
    }

    /// Releases a button at the current position.
    pub async fn up(&self, options: ClickOptions) -> Result<()> {
        let (x, y) = *self.position.lock().await;
        self.dispatch(
            input::DispatchMouseEventParamsType::MouseReleased,
            x,
            y,
            Some(options.button),
            Some(options.click_count.unwrap_or(1)),
            options.modifiers,
        )
        .await
    }

    /// Moves to `(x, y)` and clicks.
    pub async fn click(&self, x: f64, y: f64, options: ClickOptions) -> Result<()> {
        self.move_to(x, y, 1).await?;
        self.down(options).await?;
        self.up(options).await
    }

    /// Moves to `(x, y)` and double-clicks.
    pub async fn dblclick(&self, x: f64, y: f64, options: ClickOptions) -> Result<()> {
        self.move_to(x, y, 1).await?;
        for count in 1..=2 {
            let opts = ClickOptions {
                click_count: Some(count),
                ..options
            };
            self.down(opts).await?;
            self.up(opts).await?;
        }
        Ok(())
    }

    /// Scrolls by `(delta_x, delta_y)` at the current position.
    pub async fn wheel(&self, delta_x: f64, delta_y: f64) -> Result<()> {
        let (x, y) = *self.position.lock().await;
        self.session
            .send(&input::DispatchMouseEventParams {
                r#type: input::DispatchMouseEventParamsType::MouseWheel,
                x,
                y,
                delta_x: Some(delta_x),
                delta_y: Some(delta_y),
                ..mouse_defaults()
            })
            .await?;
        Ok(())
    }
}

fn mouse_defaults() -> input::DispatchMouseEventParams {
    input::DispatchMouseEventParams {
        r#type: input::DispatchMouseEventParamsType::MouseMoved,
        x: 0.0,
        y: 0.0,
        modifiers: None,
        timestamp: None,
        button: None,
        buttons: None,
        click_count: None,
        force: None,
        tangential_pressure: None,
        tilt_x: None,
        tilt_y: None,
        twist: None,
        delta_x: None,
        delta_y: None,
        pointer_type: None,
    }
}

const ALT: u32 = 1;
const CONTROL: u32 = 2;
const META: u32 = 4;
const SHIFT: u32 = 8;

#[derive(Debug, Clone)]
struct KeyDef {
    key: &'static str,
    code: &'static str,
    key_code: i64,
    location: i64,
    text: String,
    shifted: Option<Box<KeyDef>>,
}

/// US keyboard layout: (code, keyCode, key, shifted key, location).
const LAYOUT: &[(&str, i64, &str, Option<&str>, i64)] = &[
    ("Escape", 27, "Escape", None, 0),
    ("F1", 112, "F1", None, 0),
    ("F2", 113, "F2", None, 0),
    ("F3", 114, "F3", None, 0),
    ("F4", 115, "F4", None, 0),
    ("F5", 116, "F5", None, 0),
    ("F6", 117, "F6", None, 0),
    ("F7", 118, "F7", None, 0),
    ("F8", 119, "F8", None, 0),
    ("F9", 120, "F9", None, 0),
    ("F10", 121, "F10", None, 0),
    ("F11", 122, "F11", None, 0),
    ("F12", 123, "F12", None, 0),
    ("PrintScreen", 44, "PrintScreen", None, 0),
    ("ScrollLock", 145, "ScrollLock", None, 0),
    ("Pause", 19, "Pause", None, 0),
    ("Backquote", 192, "`", Some("~"), 0),
    ("Digit1", 49, "1", Some("!"), 0),
    ("Digit2", 50, "2", Some("@"), 0),
    ("Digit3", 51, "3", Some("#"), 0),
    ("Digit4", 52, "4", Some("$"), 0),
    ("Digit5", 53, "5", Some("%"), 0),
    ("Digit6", 54, "6", Some("^"), 0),
    ("Digit7", 55, "7", Some("&"), 0),
    ("Digit8", 56, "8", Some("*"), 0),
    ("Digit9", 57, "9", Some("("), 0),
    ("Digit0", 48, "0", Some(")"), 0),
    ("Minus", 189, "-", Some("_"), 0),
    ("Equal", 187, "=", Some("+"), 0),
    ("Backspace", 8, "Backspace", None, 0),
    ("Tab", 9, "Tab", None, 0),
    ("KeyQ", 81, "q", Some("Q"), 0),
    ("KeyW", 87, "w", Some("W"), 0),
    ("KeyE", 69, "e", Some("E"), 0),
    ("KeyR", 82, "r", Some("R"), 0),
    ("KeyT", 84, "t", Some("T"), 0),
    ("KeyY", 89, "y", Some("Y"), 0),
    ("KeyU", 85, "u", Some("U"), 0),
    ("KeyI", 73, "i", Some("I"), 0),
    ("KeyO", 79, "o", Some("O"), 0),
    ("KeyP", 80, "p", Some("P"), 0),
    ("BracketLeft", 219, "[", Some("{"), 0),
    ("BracketRight", 221, "]", Some("}"), 0),
    ("Backslash", 220, "\\", Some("|"), 0),
    ("CapsLock", 20, "CapsLock", None, 0),
    ("KeyA", 65, "a", Some("A"), 0),
    ("KeyS", 83, "s", Some("S"), 0),
    ("KeyD", 68, "d", Some("D"), 0),
    ("KeyF", 70, "f", Some("F"), 0),
    ("KeyG", 71, "g", Some("G"), 0),
    ("KeyH", 72, "h", Some("H"), 0),
    ("KeyJ", 74, "j", Some("J"), 0),
    ("KeyK", 75, "k", Some("K"), 0),
    ("KeyL", 76, "l", Some("L"), 0),
    ("Semicolon", 186, ";", Some(":"), 0),
    ("Quote", 222, "'", Some("\""), 0),
    ("Enter", 13, "Enter", None, 0),
    ("ShiftLeft", 16, "Shift", None, 1),
    ("KeyZ", 90, "z", Some("Z"), 0),
    ("KeyX", 88, "x", Some("X"), 0),
    ("KeyC", 67, "c", Some("C"), 0),
    ("KeyV", 86, "v", Some("V"), 0),
    ("KeyB", 66, "b", Some("B"), 0),
    ("KeyN", 78, "n", Some("N"), 0),
    ("KeyM", 77, "m", Some("M"), 0),
    ("Comma", 188, ",", Some("<"), 0),
    ("Period", 190, ".", Some(">"), 0),
    ("Slash", 191, "/", Some("?"), 0),
    ("ShiftRight", 16, "Shift", None, 2),
    ("ControlLeft", 17, "Control", None, 1),
    ("MetaLeft", 91, "Meta", None, 1),
    ("AltLeft", 18, "Alt", None, 1),
    ("Space", 32, " ", None, 0),
    ("AltRight", 18, "Alt", None, 2),
    ("MetaRight", 92, "Meta", None, 2),
    ("ContextMenu", 93, "ContextMenu", None, 0),
    ("ControlRight", 17, "Control", None, 2),
    ("Insert", 45, "Insert", None, 0),
    ("Home", 36, "Home", None, 0),
    ("PageUp", 33, "PageUp", None, 0),
    ("Delete", 46, "Delete", None, 0),
    ("End", 35, "End", None, 0),
    ("PageDown", 34, "PageDown", None, 0),
    ("ArrowUp", 38, "ArrowUp", None, 0),
    ("ArrowLeft", 37, "ArrowLeft", None, 0),
    ("ArrowDown", 40, "ArrowDown", None, 0),
    ("ArrowRight", 39, "ArrowRight", None, 0),
    ("AudioVolumeMute", 173, "AudioVolumeMute", None, 0),
    ("AudioVolumeDown", 174, "AudioVolumeDown", None, 0),
    ("AudioVolumeUp", 175, "AudioVolumeUp", None, 0),
    ("MediaTrackNext", 176, "MediaTrackNext", None, 0),
    ("MediaTrackPrevious", 177, "MediaTrackPrevious", None, 0),
    ("MediaStop", 178, "MediaStop", None, 0),
    ("MediaPlayPause", 179, "MediaPlayPause", None, 0),
    ("NumLock", 144, "NumLock", None, 0),
    ("NumpadDivide", 111, "/", None, 3),
    ("NumpadMultiply", 106, "*", None, 3),
    ("NumpadSubtract", 109, "-", None, 3),
    ("Numpad7", 103, "7", None, 3),
    ("Numpad8", 104, "8", None, 3),
    ("Numpad9", 105, "9", None, 3),
    ("Numpad4", 100, "4", None, 3),
    ("Numpad5", 101, "5", None, 3),
    ("Numpad6", 102, "6", None, 3),
    ("NumpadAdd", 107, "+", None, 3),
    ("Numpad1", 97, "1", None, 3),
    ("Numpad2", 98, "2", None, 3),
    ("Numpad3", 99, "3", None, 3),
    ("Numpad0", 96, "0", None, 3),
    ("NumpadDecimal", 110, ".", None, 3),
    ("NumpadEnter", 13, "Enter", None, 3),
];

fn text_for(key: &str) -> String {
    match key {
        "Enter" => "\r".to_owned(),
        k if k.chars().count() == 1 => k.to_owned(),
        _ => String::new(),
    }
}

static KEYS: LazyLock<HashMap<&'static str, KeyDef>> = LazyLock::new(|| {
    let mut by_name: HashMap<&'static str, KeyDef> = HashMap::new();
    for &(code, key_code, key, shift_key, location) in LAYOUT {
        let shifted = shift_key.map(|s| KeyDef {
            key: s,
            code,
            key_code,
            location,
            text: text_for(s),
            shifted: None,
        });
        let def = KeyDef {
            key,
            code,
            key_code,
            location,
            text: text_for(key),
            shifted: shifted.clone().map(Box::new),
        };
        by_name.insert(code, def.clone());
        by_name.entry(key).or_insert(def);
        if let Some(s) = shifted {
            by_name.entry(s.key).or_insert(s);
        }
    }
    let enter = by_name["Enter"].clone();
    by_name.insert("\n", enter.clone());
    by_name.insert("\r", enter);
    let space = by_name[" "].clone();
    by_name.insert("Space", space);
    by_name
});

fn resolve_key(key: &str) -> Result<KeyDef> {
    KEYS.get(key)
        .cloned()
        .ok_or_else(|| Error::invalid_argument(format!("Unknown key: '{key}'")))
}

fn modifier_bit(def: &KeyDef) -> u32 {
    match def.key {
        "Alt" => ALT,
        "Control" => CONTROL,
        "Meta" => META,
        "Shift" => SHIFT,
        _ => 0,
    }
}

/// Splits `Control+Shift+A` into its keys; a lone `+` is the plus key.
pub(crate) fn split_chord(chord: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut building = String::new();
    for c in chord.chars() {
        if c == '+' && !building.is_empty() {
            keys.push(std::mem::take(&mut building));
        } else {
            building.push(c);
        }
    }
    keys.push(building);
    keys
}

#[derive(Default)]
struct KeyboardState {
    modifiers: u32,
    pressed: HashSet<&'static str>,
}

/// The page's keyboard, with a US layout.
#[derive(Clone)]
pub struct Keyboard {
    session: Arc<dyn Session>,
    state: Arc<Mutex<KeyboardState>>,
}

impl Keyboard {
    pub(crate) fn new(session: Arc<dyn Session>) -> Self {
        Self {
            session,
            state: Arc::new(Mutex::new(KeyboardState::default())),
        }
    }

    /// Presses a key without releasing it. Modifier keys stay held for later keys.
    pub async fn down(&self, key: &str) -> Result<()> {
        let mut state = self.state.lock().await;
        let def = for_modifiers(resolve_key(key)?, state.modifiers);
        let modifiers = state.modifiers | modifier_bit(&def);
        let auto_repeat = state.pressed.contains(def.code);
        let text = if modifiers & !SHIFT != 0 {
            String::new()
        } else {
            def.text.clone()
        };
        let has_text = !text.is_empty();
        self.session
            .send(&input::DispatchKeyEventParams {
                r#type: if has_text {
                    input::DispatchKeyEventParamsType::KeyDown
                } else {
                    input::DispatchKeyEventParamsType::RawKeyDown
                },
                modifiers: Some(modifiers as i64),
                text: has_text.then(|| text.clone()),
                unmodified_text: has_text.then_some(text),
                code: Some(def.code.to_owned()),
                key: Some(def.key.to_owned()),
                windows_virtual_key_code: Some(def.key_code),
                auto_repeat: Some(auto_repeat),
                is_keypad: (def.location == 3).then_some(true),
                location: Some(def.location),
                ..key_defaults()
            })
            .await?;
        state.modifiers = modifiers;
        state.pressed.insert(def.code);
        Ok(())
    }

    /// Releases a key.
    pub async fn up(&self, key: &str) -> Result<()> {
        let mut state = self.state.lock().await;
        let def = for_modifiers(resolve_key(key)?, state.modifiers);
        state.modifiers &= !modifier_bit(&def);
        state.pressed.remove(def.code);
        self.session
            .send(&input::DispatchKeyEventParams {
                r#type: input::DispatchKeyEventParamsType::KeyUp,
                modifiers: Some(state.modifiers as i64),
                code: Some(def.code.to_owned()),
                key: Some(def.key.to_owned()),
                windows_virtual_key_code: Some(def.key_code),
                is_keypad: (def.location == 3).then_some(true),
                location: Some(def.location),
                ..key_defaults()
            })
            .await?;
        Ok(())
    }

    /// Presses and releases a key or chord, such as `Enter` or `Control+A`.
    pub async fn press(&self, chord: &str) -> Result<()> {
        let keys = split_chord(chord);
        for key in &keys {
            resolve_key(key)?;
        }
        let mut down = 0;
        let mut result = Ok(());
        for key in &keys {
            if let Err(e) = self.down(key).await {
                result = Err(e);
                break;
            }
            down += 1;
        }
        for key in keys[..down].iter().rev() {
            let released = self.up(key).await;
            if result.is_ok() {
                result = released;
            }
        }
        result
    }

    /// Types `text` one character at a time. Characters with no key on the layout are
    /// inserted as text.
    pub async fn type_text(&self, text: &str) -> Result<()> {
        for c in text.chars() {
            let s = c.to_string();
            if KEYS.contains_key(s.as_str()) {
                self.press(&s).await?;
            } else {
                self.insert_text(&s).await?;
            }
        }
        Ok(())
    }

    /// Inserts text as if pasted: no key events, just `input`.
    pub async fn insert_text(&self, text: &str) -> Result<()> {
        self.session
            .send(&input::InsertTextParams {
                text: text.to_owned(),
            })
            .await?;
        Ok(())
    }
}

fn for_modifiers(def: KeyDef, modifiers: u32) -> KeyDef {
    if modifiers & SHIFT != 0
        && let Some(shifted) = &def.shifted
    {
        return (**shifted).clone();
    }
    def
}

fn key_defaults() -> input::DispatchKeyEventParams {
    input::DispatchKeyEventParams {
        r#type: input::DispatchKeyEventParamsType::KeyDown,
        modifiers: None,
        timestamp: None,
        text: None,
        unmodified_text: None,
        key_identifier: None,
        code: None,
        key: None,
        windows_virtual_key_code: None,
        native_virtual_key_code: None,
        auto_repeat: None,
        is_keypad: None,
        is_system_key: None,
        location: None,
        commands: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeSocket;
    use crate::transport::{CdpConnection, CdpSession, ConnectionOptions};
    use serde_json::json;

    fn session() -> (FakeSocket, Arc<dyn Session>) {
        let (fake, pair) = FakeSocket::new();
        fake.respond_to("Input.dispatchKeyEvent", json!({}));
        fake.respond_to("Input.dispatchMouseEvent", json!({}));
        fake.respond_to("Input.insertText", json!({}));
        let conn = CdpConnection::start(pair, ConnectionOptions::default());
        (fake, Arc::new(CdpSession::new(conn, Some("S".into()))))
    }

    #[test]
    fn chords_split_on_plus() {
        assert_eq!(split_chord("Control+Shift+A"), ["Control", "Shift", "A"]);
        assert_eq!(split_chord("+"), ["+"]);
        assert_eq!(split_chord("Shift++"), ["Shift", "+"]);
    }

    #[test]
    fn layout_resolves_names_codes_and_shifted_keys() {
        assert_eq!(resolve_key("KeyA").unwrap().key, "a");
        assert_eq!(resolve_key("A").unwrap().code, "KeyA");
        assert_eq!(resolve_key("\n").unwrap().key, "Enter");
        assert_eq!(resolve_key("Space").unwrap().text, " ");
        assert!(resolve_key("NoSuchKey").is_err());
    }

    #[tokio::test]
    async fn press_sends_down_then_up_with_text() {
        let (fake, session) = session();
        Keyboard::new(session).press("a").await.unwrap();
        let sent = fake.sent();
        assert_eq!(sent[0]["params"]["type"], "keyDown");
        assert_eq!(sent[0]["params"]["text"], "a");
        assert_eq!(sent[1]["params"]["type"], "keyUp");
    }

    #[tokio::test]
    async fn modifier_chord_suppresses_text_and_releases_in_reverse() {
        let (fake, session) = session();
        Keyboard::new(session).press("Control+a").await.unwrap();
        let sent = fake.sent();
        let types: Vec<_> = sent
            .iter()
            .map(|m| m["params"]["type"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(types, ["rawKeyDown", "rawKeyDown", "keyUp", "keyUp"]);
        assert_eq!(sent[1]["params"]["modifiers"], 2);
        assert!(sent[1]["params"].get("text").is_none());
        assert_eq!(sent[2]["params"]["key"], "a");
        assert_eq!(sent[3]["params"]["key"], "Control");
    }

    #[tokio::test]
    async fn type_inserts_characters_off_the_layout() {
        let (fake, session) = session();
        Keyboard::new(session).type_text("é").await.unwrap();
        assert_eq!(fake.sent_methods(), ["Input.insertText"]);
    }

    #[tokio::test]
    async fn click_moves_presses_and_releases() {
        let (fake, session) = session();
        Mouse::new(session)
            .click(10.0, 20.0, ClickOptions::default())
            .await
            .unwrap();
        let sent = fake.sent();
        let types: Vec<_> = sent
            .iter()
            .map(|m| m["params"]["type"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(types, ["mouseMoved", "mousePressed", "mouseReleased"]);
        assert_eq!(sent[1]["params"]["x"], 10.0);
        assert_eq!(sent[1]["params"]["button"], "left");
        assert_eq!(sent[1]["params"]["clickCount"], 1);
    }
}
