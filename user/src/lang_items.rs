use super::exit;

#[panic_handler]
fn panic_handler(panic_info: &core::panic::PanicInfo) -> ! {
    match (panic_info.location(), panic_info.message()) {
        (Some(location), Some(message)) => {
            println!(
                "Panicked at {}:{}, {}",
                location.file(),
                location.line(),
                message
            );
        }
        (Some(location), None) => {
            println!("Panicked at {}:{}", location.file(), location.line());
        }
        (None, Some(message)) => {
            println!("Panicked: {}", message);
        }
        (None, None) => {
            println!("Panicked");
        }
    }
    exit(-1);
}
