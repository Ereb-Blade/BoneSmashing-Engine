use std::collections::HashSet;
use winit::keyboard::KeyCode;

#[derive(Default)]
pub struct Input {
    keys: HashSet<KeyCode>,
    pub mouse_delta: (f64, f64),
    pub cursor_locked: bool,
}

impl Input {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn process_key(&mut self, keycode: KeyCode, pressed: bool) {
        if pressed {
            self.keys.insert(keycode);
        } else {
            self.keys.remove(&keycode);
        }
    }

    pub fn process_mouse_motion(&mut self, delta: (f64, f64)) {
        self.mouse_delta.0 += delta.0;
        self.mouse_delta.1 += delta.1;
    }

    pub fn is_key_pressed(&self, keycode: KeyCode) -> bool {
        self.keys.contains(&keycode)
    }

    pub fn reset_mouse_delta(&mut self) {
        self.mouse_delta = (0.0, 0.0);
    }

    pub fn clear(&mut self) {
        self.keys.clear();
        self.reset_mouse_delta();
    }
}
