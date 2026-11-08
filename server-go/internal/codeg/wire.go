package codeg

func modeWire(value any) any {
	m, ok := value.(map[string]any)
	if !ok {
		return nil
	}
	return Row{"current_mode_id": m["currentModeId"], "available_modes": m["availableModes"]}
}
func configWire(v Row) []Row {
	out := []Row{}
	if opts, ok := v["configOptions"].([]any); ok {
		for _, raw := range opts {
			o, ok := raw.(map[string]any)
			if !ok {
				continue
			}
			options := []any{}
			if list, ok := o["options"].([]any); ok {
				for _, option := range list {
					item, _ := option.(map[string]any)
					if group, ok := item["options"].([]any); ok {
						options = append(options, group...)
					} else {
						options = append(options, option)
					}
				}
			}
			out = append(out, Row{"id": o["id"], "name": o["name"], "category": o["category"], "description": o["description"], "kind": Row{"type": "select", "current_value": o["currentValue"], "options": options, "groups": []Row{}}})
		}
	}
	if len(out) == 0 {
		if models, ok := v["models"].(map[string]any); ok {
			options := []Row{}
			if list, ok := models["availableModels"].([]any); ok {
				for _, raw := range list {
					model, _ := raw.(map[string]any)
					options = append(options, Row{"value": model["modelId"], "name": model["name"]})
				}
			}
			out = append(out, Row{"id": "model", "name": "Model", "category": "model", "kind": Row{"type": "select", "current_value": models["currentModelId"], "options": options, "groups": []Row{}}})
		}
	}
	return out
}
func permissionOptions(value any) []Row {
	out := []Row{}
	if list, ok := value.([]any); ok {
		for _, raw := range list {
			o, _ := raw.(map[string]any)
			out = append(out, Row{"option_id": o["optionId"], "name": o["name"], "kind": o["kind"]})
		}
	}
	return out
}
