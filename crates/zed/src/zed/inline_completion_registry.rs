use client::Client;
use collections::HashMap;
use editor::Editor;
use gpui::{AnyWindowHandle, App, Context, WeakEntity};
use language::language_settings::{EditPredictionProvider, all_language_settings};
use std::{cell::RefCell, rc::Rc, sync::Arc};
use ui::Window;

pub fn init(client: Arc<Client>, cx: &mut App) {
    let editors: Rc<RefCell<HashMap<WeakEntity<Editor>, AnyWindowHandle>>> = Rc::default();
    cx.observe_new({
        let editors = editors.clone();
        let client = client.clone();
        move |editor: &mut Editor, window, cx: &mut Context<Editor>| {
            if !editor.mode().is_full() {
                return;
            }

            let Some(window) = window else {
                return;
            };

            let editor_handle = cx.entity().downgrade();
            cx.on_release({
                let editor_handle = editor_handle.clone();
                let editors = editors.clone();
                move |_, _| {
                    editors.borrow_mut().remove(&editor_handle);
                }
            })
            .detach();

            editors
                .borrow_mut()
                .insert(editor_handle, window.window_handle());
            let provider = all_language_settings(None, cx).edit_predictions.provider;
            assign_edit_prediction_provider(editor, provider, &client, window, cx);
        }
    })
    .detach();

    let provider = all_language_settings(None, cx).edit_predictions.provider;
    for (editor, window) in editors.borrow().iter() {
        _ = window.update(cx, |_window, window, cx| {
            _ = editor.update(cx, |editor, cx| {
                assign_edit_prediction_provider(editor, provider, &client, window, cx);
            })
        });
    }
}

fn assign_edit_prediction_provider(
    _: &mut Editor,
    provider: EditPredictionProvider,
    _: &Arc<Client>,
    _: &mut Window,
    _: &mut Context<Editor>,
) {
    match provider {
        EditPredictionProvider::None => {}
    }
}
