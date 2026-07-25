#![no_std]
#![no_main]
extern crate alloc;

#[path = "test_runner/mod.rs"] mod runner;

#[no_mangle]
fn main(argc: usize, argv: &[&str]) -> i32 {
    runner::main(argc, argv)
}
