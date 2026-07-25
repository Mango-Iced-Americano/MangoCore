use super::exit;

#[panic_handler]
fn panic_handler(panic_info: &core::panic::PanicInfo) -> ! {
    match panic_info.location() {
        Some(location) => {
            println!(
                "Panicked at {}:{}, {}",
                location.file(),
                location.line(),
                panic_info.message()
            );
        }
        None => {
            println!("Panicked: {}", panic_info.message());
        }
    }
    exit(-1);
}
