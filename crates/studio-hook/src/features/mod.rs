pub mod editor_background;
pub mod window_transparency;

pub fn init() {
    crate::guard("window_transparency", window_transparency::init);
    crate::guard("editor_background", editor_background::init);
}
