pub mod window_transparency;

pub fn init() {
    crate::guard("window_transparency", window_transparency::init);
}
