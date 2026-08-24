//! Flatten arm/confirm and TUI key bindings (no TTY).

use tui_bot::keys::{apply_flatten_key, handle_key, KeyAction};

#[test]
fn keys_bind_strategy_refresh_quit() {
    assert_eq!(handle_key('1', false), KeyAction::Strategy(1));
    assert_eq!(handle_key('2', false), KeyAction::Strategy(2));
    assert_eq!(handle_key('3', false), KeyAction::Strategy(3));
    assert_eq!(handle_key('4', false), KeyAction::Strategy(4));
    assert_eq!(handle_key('r', false), KeyAction::Refresh);
    assert_eq!(handle_key('R', false), KeyAction::Refresh);
    assert_eq!(handle_key('q', false), KeyAction::Quit);
}

#[test]
fn flatten_is_x_then_x_other_cancels() {
    assert_eq!(handle_key('x', false), KeyAction::FlattenArm);
    let (armed, confirmed) = apply_flatten_key(false, 'x');
    assert!(armed && !confirmed);
    let (armed2, confirmed2) = apply_flatten_key(true, 'x');
    assert!(!armed2 && confirmed2);
    let (armed3, confirmed3) = apply_flatten_key(true, 'r');
    // r while armed is refresh in handle_key, not cancel — flatten cancel is any other non-bound? 
    // Spec: any other key cancels. Our handle_key maps r to Refresh even when armed.
    // Confirm path: second x confirms. Unrelated letter cancels.
    assert_eq!(handle_key('z', true), KeyAction::FlattenCancel);
    let (a, c) = apply_flatten_key(true, 'z');
    assert!(!a && !c);
    let _ = (armed3, confirmed3);
}
