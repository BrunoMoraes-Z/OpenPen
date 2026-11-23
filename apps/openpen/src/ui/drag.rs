//! Window-wide pointer capture for drags that may leave the element that
//! started them (panel resize, scrub on field labels).

use std::rc::Rc;

use gpui_kit::*;

type MoveFn<T> = dyn Fn(&mut T, &MouseMoveEvent, &mut Window, &mut Context<T>);
type UpFn<T> = dyn Fn(&mut T, &mut Window, &mut Context<T>);

/// Zero-size element that, while rendered, routes every mouse move / left
/// button release in the window to `entity`. Render it only while a drag is
/// active.
pub fn capture<T: 'static>(
    entity: Entity<T>,
    cursor: CursorStyle,
    on_move: impl Fn(&mut T, &MouseMoveEvent, &mut Window, &mut Context<T>) + 'static,
    on_up: impl Fn(&mut T, &mut Window, &mut Context<T>) + 'static,
) -> impl IntoElement {
    let on_move: Rc<MoveFn<T>> = Rc::new(on_move);
    let on_up: Rc<UpFn<T>> = Rc::new(on_up);
    canvas(
        |_, _, _| (),
        move |_, _, window, _| {
            window.set_window_cursor_style(cursor);
            let e = entity.clone();
            let f = on_move.clone();
            window.on_mouse_event(move |ev: &MouseMoveEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble {
                    e.update(cx, |t, cx| f(t, ev, window, cx));
                }
            });
            let e = entity.clone();
            let f = on_up.clone();
            window.on_mouse_event(move |ev: &MouseUpEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble && ev.button == MouseButton::Left {
                    e.update(cx, |t, cx| f(t, window, cx));
                }
            });
        },
    )
    .absolute()
    .size_0()
}

/// Width of a side panel after dragging its edge by `dx` (positive = right).
/// `sign` is +1 for a left panel (handle on its right edge) and -1 for a right
/// panel.
pub fn resized_width(start: f32, dx: f32, sign: f32, min: f32, max: f32) -> f32 {
    (start + dx * sign).clamp(min, max)
}

#[cfg(test)]
mod tests {
    use super::resized_width;

    #[test]
    fn panel_resize_math() {
        assert_eq!(resized_width(240.0, 30.0, 1.0, 200.0, 480.0), 270.0);
        assert_eq!(resized_width(240.0, 30.0, -1.0, 200.0, 480.0), 210.0);
        assert_eq!(resized_width(240.0, -100.0, 1.0, 200.0, 480.0), 200.0);
        assert_eq!(resized_width(240.0, -900.0, -1.0, 200.0, 480.0), 480.0);
    }
}
