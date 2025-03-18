#!/usr/bin/fish

set -l crates acp_thread agent agent_ui agent_settings ai_onboarding assistant_context assistant_slash_commands assistant_tool assistant_tools auto_update_ui call collab collab_ui copilot livekit_client lmstudio language_model mistral eval prompt_store zeta x_ai supermaven language_models context_server zeta vercel web_search_providers

for crate in $crates
    rm -rf ./crates/$crate
    git add ./crates/$crate
end
