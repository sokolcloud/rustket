//! Start on login (`~/.config/autostart/rustket.desktop`). STUB.

#[derive(Debug, Default)]
pub struct Autostart;

impl Autostart {
    pub fn is_enabled(&self) -> bool {
        false
    }

    pub fn set_enabled(&mut self, enabled: bool) -> Result<(), String> {
        let _ = enabled;
        Err("not implemented".into())
    }
}
