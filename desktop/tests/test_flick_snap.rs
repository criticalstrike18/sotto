// On macOS, `winit::EventLoop` must run on OS thread #1 rather than a `cargo test` worker thread.
#![cfg(not(target_os = "macos"))]

#[test]
fn test_flickable_offset_without_content_height() -> Result<(), Box<dyn std::error::Error>> {
    std::env::set_var("SLINT_BACKEND", "winit-software");
    slint::slint! {
        export component TestWin inherits Window {
            width: 400px;
            height: 400px;
            in-out property <length> scroll_y: 0px;

            flick := Flickable {
                interactive: false;
                width: parent.width;
                height: parent.height;
                content-y: root.scroll_y;

                VerticalLayout {
                    alignment: start;
                    Rectangle {
                        height: 100px;
                        background: red;
                    }
                    Rectangle {
                        height: 100px;
                        background: blue;
                    }
                }
            }
        }
    }

    let win = TestWin::new()?;
    win.show()?;
    let snap1 = win.window().take_snapshot()?;
    win.set_scroll_y(-50.0);
    let snap2 = win.window().take_snapshot()?;

    let p1 = snap1.as_slice();
    let p2 = snap2.as_slice();
    let mut diff = 0;
    for i in 0..p1.len() {
        if p1[i] != p2[i] { diff += 1; }
    }
    println!("Diff WITHOUT content-height set: {}", diff);
    Ok(())
}
