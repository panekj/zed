use std::{sync::Arc, time::Duration};

use crate::onboarding_event;
use client::Client;
use gpui::{
    ease_in_out, svg, Animation, AnimationExt as _, ClickEvent, DismissEvent, EventEmitter,
    FocusHandle, Focusable, MouseDownEvent, Render,
};
use settings::Settings;
use ui::{prelude::*, TintColor};
use workspace::{notifications::NotifyTaskExt, ModalView, Workspace};

/// Introduces user to Zed's Edit Prediction feature and terms of service
pub struct ZedPredictModal {
    client: Arc<Client>,
    focus_handle: FocusHandle,
    sign_in_status: SignInStatus,
    data_collection_expanded: bool,
}

#[derive(PartialEq, Eq)]
enum SignInStatus {
    /// Signed out or signed in but not from this modal
    Idle,
    /// Authentication triggered from this modal
    Waiting,
    /// Signed in after authentication from this modal
    SignedIn,
}

impl ZedPredictModal {
    pub fn toggle(
        workspace: &mut Workspace,
        _: (),
        client: Arc<Client>,
        _: (),
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        workspace.toggle_modal(window, cx, |_window, cx| Self {
            client,
            focus_handle: cx.focus_handle(),
            sign_in_status: SignInStatus::Idle,
            data_collection_expanded: false,
        });
    }

    fn view_blog(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        cx.open_url("https://zed.dev/blog/edit-prediction");
        cx.notify();

        onboarding_event!("Blog Link clicked");
    }

    fn sign_in(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.client.clone();
        self.sign_in_status = SignInStatus::Waiting;

        cx.spawn(async move |this, cx| {
            let result = client.authenticate_and_connect(true, &cx).await;

            let status = match result {
                Ok(_) => SignInStatus::SignedIn,
                Err(_) => SignInStatus::Idle,
            };

            this.update(cx, |this, cx| {
                this.sign_in_status = status;
                onboarding_event!("Signed In");
                cx.notify()
            })?;

            result
        })
        .detach_and_notify_err(window, cx);

        onboarding_event!("Sign In Clicked");
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }
}

impl EventEmitter<DismissEvent> for ZedPredictModal {}

impl Focusable for ZedPredictModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ModalView for ZedPredictModal {}

impl Render for ZedPredictModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let window_height = window.viewport_size().height;
        let max_height = window_height - px(200.);

        let base = v_flex()
            .id("edit-prediction-onboarding")
            .key_context("ZedPredictModal")
            .relative()
            .w(px(550.))
            .h_full()
            .max_h(max_height)
            .p_4()
            .gap_2()
            .when(self.data_collection_expanded, |element| {
                element.overflow_y_scroll()
            })
            .when(!self.data_collection_expanded, |element| {
                element.overflow_hidden()
            })
            .elevation_3(cx)
            .track_focus(&self.focus_handle(cx))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(|_, _: &menu::Cancel, _window, cx| {
                onboarding_event!("Cancelled", trigger = "Action");
                cx.emit(DismissEvent);
            }))
            .on_any_mouse_down(cx.listener(|this, _: &MouseDownEvent, window, _cx| {
                this.focus_handle.focus(window);
            }))
            .child(
                div()
                    .p_1p5()
                    .absolute()
                    .top_1()
                    .left_1()
                    .right_0()
                    .h(px(200.))
                    .child(
                        svg()
                            .path("icons/zed_predict_bg.svg")
                            .text_color(cx.theme().colors().icon_disabled)
                            .w(px(530.))
                            .h(px(128.))
                            .overflow_hidden(),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .mb_2()
                    .justify_between()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                Label::new("Introducing Zed AI's")
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            )
                            .child(Headline::new("Edit Prediction").size(HeadlineSize::Large)),
                    )
                    .child({
                        let tab = |n: usize| {
                            let text_color = cx.theme().colors().text;
                            let border_color = cx.theme().colors().text_accent.opacity(0.4);

                            h_flex().child(
                                h_flex()
                                    .px_4()
                                    .py_0p5()
                                    .bg(cx.theme().colors().editor_background)
                                    .border_1()
                                    .border_color(border_color)
                                    .rounded_sm()
                                    .font(theme::ThemeSettings::get_global(cx).buffer_font.clone())
                                    .text_size(TextSize::XSmall.rems(cx))
                                    .text_color(text_color)
                                    .child("tab")
                                    .with_animation(
                                        ElementId::Integer(n),
                                        Animation::new(Duration::from_secs(2)).repeat(),
                                        move |tab, delta| {
                                            let delta = (delta - 0.15 * n as f32) / 0.7;
                                            let delta = 1.0 - (0.5 - delta).abs() * 2.;
                                            let delta = ease_in_out(delta.clamp(0., 1.));
                                            let delta = 0.1 + 0.9 * delta;

                                            tab.border_color(border_color.opacity(delta))
                                                .text_color(text_color.opacity(delta))
                                        },
                                    ),
                            )
                        };

                        v_flex()
                            .gap_2()
                            .items_center()
                            .pr_2p5()
                            .child(tab(0).ml_neg_20())
                            .child(tab(1))
                            .child(tab(2).ml_20())
                    }),
            )
            .child(h_flex().absolute().top_2().right_2().child(
                IconButton::new("cancel", IconName::X).on_click(cx.listener(
                    |_, _: &ClickEvent, _window, cx| {
                        onboarding_event!("Cancelled", trigger = "X click");
                        cx.emit(DismissEvent);
                    },
                )),
            ));

        let blog_post_button = Button::new("view-blog", "Read the Blog Post")
            .full_width()
            .icon(IconName::ArrowUpRight)
            .icon_size(IconSize::Indicator)
            .icon_color(Color::Muted)
            .on_click(cx.listener(Self::view_blog));

        base.child(Label::new("Edit prediction provider unavailable").color(Color::Muted))
            .child(
                v_flex()
                    .mt_2()
                    .gap_2()
                    .w_full()
                    .child(
                        Button::new("accept-tos", "Sign in with GitHub")
                            .disabled(true)
                            .style(ButtonStyle::Tinted(TintColor::Accent))
                            .full_width()
                            .on_click(cx.listener(Self::sign_in)),
                    )
                    .child(blog_post_button),
            )
    }
}
