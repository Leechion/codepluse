package codeg

import (
	"strings"
	"testing"

	"harnesshub/internal/hub"
)

// 原版把两条错误语义当成前端契约：AcpError::SdkNotInstalled 的文案必须原样含
// "is not installed"（connection.rs:2358-2361 标 INVARIANT，前端
// acp-connections-context.tsx:7733 靠它弹安装引导），以及注册丢失时的
// "is no longer registered"。这里把两条钉住，防止后续中文化把它们吃掉。
func TestCustomAgentLaunchErrorsKeepFrontendInvariants(t *testing.T) {
	s := testServer(t)

	lost := &hub.Agent{ID: "agt_custom_not-there", Wire: "custom:not-there", Name: "Not There", Real: true}
	launch, ok, err := s.CustomAgentLaunch(lost)
	if !ok || err == nil {
		t.Fatalf("a custom: wire without a stored definition must be an explicit failure, got ok=%v err=%v", ok, err)
	}
	if launch != nil {
		t.Fatal("no launch may accompany the failure")
	}
	if !strings.Contains(err.Error(), "is no longer registered") {
		t.Fatalf("lost-registration message must keep the original phrase, got %q", err.Error())
	}

	// harness 实例的 Wire 为空，wireAgent 会兜底成 "custom:<ID>"（server.go:139）。
	// 判定必须只看 agent.Wire 本身，否则这类实例会被误判成注册丢失，再也走不到
	// customAcp 那条启动路径。
	instance := &hub.Agent{ID: "agt_instance_01", Name: "Instance", Real: true}
	if _, ok, err = s.CustomAgentLaunch(instance); ok {
		t.Fatalf("an instance without an explicit custom: wire must fall through, got ok=%v err=%v", ok, err)
	}

	if msg := customNotInstalled("Probe Agent", nil).Error(); !strings.Contains(msg, "is not installed") {
		t.Fatalf("missing-install message must keep the invariant substring, got %q", msg)
	}
	if msg := customNotInstalled("Probe Agent", err).Error(); !strings.Contains(msg, "is not installed") {
		t.Fatalf("detailed variant must keep the invariant substring too, got %q", msg)
	}
}
