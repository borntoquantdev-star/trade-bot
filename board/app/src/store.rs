//! Where the board keeps the layout somebody arranged.
//!
//! One string, saved in the obvious place for each build: `localStorage` in
//! the browser, a file in the working directory natively. Written by hand
//! rather than through eframe's persistence feature, which on 0.37 wants a
//! `ron` this machine's index does not carry - and which would have brought
//! a serialisation format along for a job that is already JSON.
//!
//! **Nothing here is allowed to fail loudly.** A browser with storage turned
//! off, a read-only folder, a file somebody edited into nonsense: every one
//! of those means "no saved layout", which is exactly the state the board
//! starts in anyway. A board that refuses to open because it could not
//! remember where a card was is worse than a board that forgets.

/// The file natively, and the key in the browser.
const NAME: &str = "board-layout.json";

#[cfg(target_arch = "wasm32")]
fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

/// The layout from last time, if there is one.
pub fn load() -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        storage()?.get_item(NAME).ok()?
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        read(&path())
    }
}

/// Keep it for next time. Quietly does nothing if it cannot.
pub fn save(text: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(storage) = storage() {
            let _ = storage.set_item(NAME, text);
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        write(&path(), text);
    }
}

/// Forget it, for the button that puts every card back.
pub fn clear() {
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(storage) = storage() {
            let _ = storage.remove_item(NAME);
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        forget(&path());
    }
}

// The three file operations, each taking the path rather than finding it.
// A test can then use a file of its own instead of reaching for the
// environment, which two tests running at once cannot share.

#[cfg(not(target_arch = "wasm32"))]
fn read(path: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[cfg(not(target_arch = "wasm32"))]
fn write(path: &std::path::Path, text: &str) {
    let _ = std::fs::write(path, text);
}

#[cfg(not(target_arch = "wasm32"))]
fn forget(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
}

/// `BOARD_LAYOUT` if it is set, otherwise beside wherever the board was
/// started from.
#[cfg(not(target_arch = "wasm32"))]
fn path() -> std::path::PathBuf {
    std::env::var("BOARD_LAYOUT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(NAME))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// A file of this test's own. Tests run at the same time in one
    /// process, so anything shared - a fixed filename, an environment
    /// variable - is a test that fails only when the suite is busy.
    fn scratch(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("board-layout-{}-{name}.json", std::process::id()))
    }

    #[test]
    fn what_is_saved_comes_back() {
        let file = scratch("round-trip");
        write(
            &file,
            "{\"market\":{\"x\":1.0,\"y\":2.0,\"w\":3.0,\"h\":4.0}}",
        );
        assert!(read(&file).unwrap().contains("market"));

        forget(&file);
        assert_eq!(read(&file), None, "ลบแล้วต้องไม่เหลือ");
    }

    #[test]
    fn nothing_saved_is_not_an_error() {
        let file = scratch("never-written");
        forget(&file);
        assert_eq!(read(&file), None);
    }

    #[test]
    fn a_folder_that_cannot_be_written_to_is_not_a_crash() {
        // Saving into a directory that does not exist is the shape of every
        // real failure here: read-only media, a deleted working directory,
        // no permission. None of them may take the board down.
        let nowhere = std::env::temp_dir()
            .join("board-no-such-folder")
            .join("deep")
            .join(NAME);
        write(&nowhere, "{}");
        assert_eq!(read(&nowhere), None);
    }

    #[test]
    fn the_environment_can_point_the_layout_somewhere_else() {
        // Reads the variable without setting it, so it cannot collide with
        // another test: the default is what matters here.
        if std::env::var("BOARD_LAYOUT").is_err() {
            assert_eq!(path(), std::path::PathBuf::from(NAME));
        }
    }
}
