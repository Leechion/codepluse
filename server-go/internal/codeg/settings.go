package codeg

var settingDefaults = map[string]Row{
	"get_system_language_settings":  {"mode": "manual", "language": "zh_cn"},
	"get_system_terminal_settings":  {"default_shell": nil, "colorize_command_output": false},
	"get_system_rendering_settings": {"disable_hardware_acceleration": false},
	"get_feedback_settings":         {"enabled": true},
	"get_question_settings":         {"enabled": false},
	"get_session_info_settings":     {"enabled": false},
	"get_browser_tools_settings":    {"enabled": false, "eval_enabled": false},
	"get_chat_authoring_settings":   {"enabled": false},
	"get_conversation_branch_tag":   {"enabled": false, "color": "default"},
	"get_delegation_settings":       {"enabled": true, "depth_limit": 4, "completed_cache_max_mb": 32, "agent_defaults": Row{}},
}
var settingUpdates = map[string]string{
	"update_system_language_settings":  "get_system_language_settings",
	"update_system_rendering_settings": "get_system_rendering_settings",
	"update_system_terminal_settings":  "get_system_terminal_settings",
}
