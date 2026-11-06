package mcpserver

import (
	"strings"
	"testing"
)

func TestValidateRequired(t *testing.T) {
	sink := NewSink([]byte(`{"type":"object","required":["output"],"properties":{"output":{"type":"string"}}}`))
	if _, err := sink.Call(map[string]any{"output": "ok"}); err != nil {
		t.Fatalf("合法提交被拒绝：%v", err)
	}
	_, err := sink.Call(map[string]any{"wrong": 1})
	if err == nil || !strings.Contains(err.Error(), "缺少必填字段 output") {
		t.Fatalf("缺字段应被拒绝：%v", err)
	}
}

func TestValidateTypes(t *testing.T) {
	sink := NewSink([]byte(`{"type":"object","required":["output","complete"],"properties":{"output":{"type":"string"},"complete":{"type":"boolean"}}}`))
	_, err := sink.Call(map[string]any{"output": 1, "complete": true})
	if err == nil || !strings.Contains(err.Error(), "必须是字符串") {
		t.Fatalf("类型错误应被指出：%v", err)
	}
	_, err = sink.Call(map[string]any{"output": "x", "complete": "yes"})
	if err == nil || !strings.Contains(err.Error(), "必须是布尔值") {
		t.Fatalf("布尔类型校验失败：%v", err)
	}
}

func TestRefusalCap(t *testing.T) {
	sink := NewSink([]byte(`{"type":"object","required":["output"],"properties":{"output":{"type":"string"}}}`))
	var lastErr error
	for i := 0; i < maxRefused; i++ {
		_, lastErr = sink.Call(map[string]any{"bad": 1})
		if lastErr == nil {
			t.Fatalf("第 %d 次应继续拒绝", i+1)
		}
	}
	// after MAX_REFUSED refusals the sink lets submissions through
	res, err := sink.Call(map[string]any{"bad": 1})
	if err != nil || res["received"] != true {
		t.Fatalf("超限后应放行：%v %v", err, res)
	}
	if res["note"] == "" {
		t.Fatalf("放行结果应带说明")
	}
}

func TestEmptyArgsRejected(t *testing.T) {
	sink := NewSink(nil)
	if _, err := sink.Call(map[string]any{}); err == nil {
		t.Fatalf("空参数应被拒绝")
	}
}

func TestEnumValidation(t *testing.T) {
	sink := NewSink([]byte(`{"type":"object","required":["action"],"properties":{"action":{"type":"string","enum":["reply","execute","steer"]}}}`))
	if _, err := sink.Call(map[string]any{"action": "reply"}); err != nil {
		t.Fatalf("枚举内取值应放行：%v", err)
	}
	if _, err := sink.Call(map[string]any{"action": "dance"}); err == nil {
		t.Fatalf("枚举外取值应被拒绝")
	}
}
