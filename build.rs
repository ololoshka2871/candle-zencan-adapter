pub fn main() -> Result<(), Box<dyn std::error::Error>>{
    match std::env::var("CARGO_CFG_TARGET_OS") {
        Ok(val) if val == "windows" => Ok(()),
        _ => Err("Building for anything but Windows is not supported by this crate".into()),
    }
}
