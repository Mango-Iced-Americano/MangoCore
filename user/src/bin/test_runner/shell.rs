use user_lib::{chdir, exec, exit, fork, wait};
pub fn enter_shell(path: &str, environ: &[*const u8]) {
    if fork() == 0 { let _ = chdir("/\0"); exec(path, &[path.as_ptr(), core::ptr::null()], environ); exec("/bash\0", &["/bash\0".as_ptr(), core::ptr::null()], environ); exit(127); }
    else { let mut status = 0; while wait(&mut status) > 0 {} }
}
