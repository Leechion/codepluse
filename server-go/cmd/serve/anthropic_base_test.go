package main

import "testing"

func TestAnthropicBaseURLTrimsTheEndpointTheSDKAppends(t *testing.T) {
	cases := map[string]string{
		// 讯飞 MaaS: CC Switch stores the full endpoint.
		"https://maas-api.cn-huabei-1.xf-yun.com/anthropic/v1/messages":  "https://maas-api.cn-huabei-1.xf-yun.com/anthropic",
		"https://maas-api.cn-huabei-1.xf-yun.com/anthropic/v1/messages/": "https://maas-api.cn-huabei-1.xf-yun.com/anthropic",
		"  https://example.test/anthropic/V1/MESSAGES ":                  "https://example.test/anthropic",
		"https://example.test/anthropic/messages":                        "https://example.test/anthropic",
		// A normal base is already what the SDK wants.
		"https://api.deepseek.com/anthropic": "https://api.deepseek.com/anthropic",
		"https://api.apigoto.com/":           "https://api.apigoto.com",
		"":                                   "",
	}
	for input, want := range cases {
		if got := anthropicBaseURL(input); got != want {
			t.Fatalf("anthropicBaseURL(%q) = %q, want %q", input, got, want)
		}
	}
}
