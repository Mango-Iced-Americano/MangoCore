//! Path normalization: split by '/' and resolve '.' / '..' components.
//!
//! Extracted from `os/src/fs/mod.rs::parse_path()`.
//! Pure logic — no kernel dependencies, no I/O, no global state.

/// Tokenize and normalize a path string.
///
/// Splits on `/`, discards empty and `"."` segments, and resolves `".."`
/// by popping the previous non-`".."` component. Leading `/` is effectively
/// represented by the _absence_ of a parent for the first real component;
/// callers track absolute vs. relative through the returned slice.
///
/// # Examples
///
/// ```
/// let v = mango_kernel_core::path::parse_path("a/b/../c");
/// assert_eq!(v, ["a", "c"]);
/// ```
pub fn parse_path(path: &str) -> alloc::vec::Vec<alloc::string::String> {
    path.split('/')
        .fold(alloc::vec::Vec::with_capacity(8), |mut v, s| {
            match s {
                "" | "." => {}
                ".." => {
                    if v.last().map_or(true, |s| s == "..") {
                        v.push(alloc::string::String::from(s));
                    } else {
                        v.pop();
                    }
                }
                _ => v.push(alloc::string::String::from(s)),
            }
            v
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    #[test]
    fn empty_string() {
        assert!(parse_path("").is_empty());
    }

    #[test]
    fn current_dir() {
        assert!(parse_path(".").is_empty());
    }

    #[test]
    fn parent_dir() {
        assert_eq!(parse_path(".."), vec![String::from("..")]);
    }

    #[test]
    fn two_components() {
        assert_eq!(parse_path("a/b"), vec![String::from("a"), String::from("b")]);
    }

    #[test]
    fn consecutive_slashes_normalized() {
        assert_eq!(parse_path("a//b"), vec![String::from("a"), String::from("b")]);
    }

    #[test]
    fn absolute_path() {
        assert_eq!(parse_path("/a/b"), vec![String::from("a"), String::from("b")]);
    }

    #[test]
    fn trailing_slash_stripped() {
        assert_eq!(parse_path("/a/b/"), vec![String::from("a"), String::from("b")]);
    }

    #[test]
    fn multiple_parents() {
        assert_eq!(
            parse_path("../../x"),
            vec![
                String::from(".."),
                String::from(".."),
                String::from("x"),
            ]
        );
    }

    #[test]
    fn mixed_dotdot() {
        assert_eq!(parse_path("a/b/../c"), vec![String::from("a"), String::from("c")]);
    }

    #[test]
    fn double_dotdot_does_not_escape_root() {
        // "a/../../b": "a" is pushed, ".." pops "a", ".." stays (last is now ".."),
        // "b" is pushed → [.., b]
        assert_eq!(
            parse_path("a/../../b"),
            vec![String::from(".."), String::from("b")]
        );
    }

    #[test]
    fn only_dots() {
        assert!(parse_path("././.").is_empty());
    }

    #[test]
    fn single_component() {
        assert_eq!(parse_path("foo"), vec![String::from("foo")]);
    }
}
