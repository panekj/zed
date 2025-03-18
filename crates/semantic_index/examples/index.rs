use client::Client;
use gpui::Application;
use http_client::HttpClientWithUrl;
use language::language_settings::AllLanguageSettings;
use project::Project;
use settings::SettingsStore;
use std::sync::Arc;

fn main() {
    zlog::init();

    use clock::FakeSystemClock;

    Application::new().run(|cx| {
        let store = SettingsStore::test(cx);
        cx.set_global(store);
        language::init(cx);
        Project::init_settings(cx);
        SettingsStore::update(cx, |store, cx| {
            store.update_user_settings::<AllLanguageSettings>(cx, |_| {});
        });

        let clock = Arc::new(FakeSystemClock::new());

        let http = Arc::new(HttpClientWithUrl::new(
            Arc::new(
                reqwest_client::ReqwestClient::user_agent("Zed semantic index example").unwrap(),
            ),
            "http://localhost:11434",
            None,
        ));
        let client = client::Client::new(clock, http.clone(), cx);
        Client::set_global(client.clone(), cx);

        let args: Vec<String> = std::env::args().collect();
        if args.len() < 2 {
            eprintln!("Usage: cargo run --example index -p semantic_index -- <project_path>");
            cx.quit();
            return;
        }
    });
}
