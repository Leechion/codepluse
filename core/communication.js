'use strict';

// Transport is explicit. Simulation messages and generated replies never trigger
// another model call; each real request has exactly one delivery attempt.
class Communication {
  constructor(hub, resolveSender) {
    this.hub = hub;
    this.resolveSender = resolveSender;
  }

  async send(spec) {
    const hub = this.hub;
    if (!spec.to || spec.to === 'hub' || spec.to === 'broadcast' || spec.to[0] === '#') {
      throw new Error('真实通信需要指定一个 Harness 实例');
    }
    if (typeof spec.text !== 'string' || !spec.text.trim() || spec.text.length > 20000) {
      throw new Error('消息内容须为 1–20000 字符');
    }
    if (!Number.isInteger(spec.sensitivity == null ? 1 : spec.sensitivity) || spec.sensitivity < 0 || spec.sensitivity > 3) {
      throw new Error('无效的消息密级');
    }
    const message = hub.sendMessage(spec);
    if (message.status === 'rejected') return message;
    message.correlationId = message.correlationId || message.id;
    message.delivery = { status: 'sending', startedAt: Date.now() };
    hub.emit('message', message);
    try {
      const target = hub.agents.get(message.to);
      const sender = this.resolveSender(target);
      const result = await sender(message);
      if (!result || !result.ok) throw new Error((result && result.error) || 'Harness 未确认接收消息');
      if (result.output) {
        const reply = hub.sendMessage({
          from: target.id, to: message.from, kind: 'collab.reply',
          text: result.output, sensitivity: message.scope.sensitivity,
          sessionId: message.scope.sessionId, correlationId: message.correlationId,
          replyTo: message.id
        });
        message.delivery.replyId = reply.id;
      }
      message.delivery.status = 'delivered';
    } catch (error) {
      message.delivery.status = 'failed';
      message.delivery.error = error.message;
    }
    message.delivery.finishedAt = Date.now();
    hub.audit({ actor: message.from, action: 'message.transport', target: message.to,
      decision: message.delivery.status === 'delivered' ? 'allowed' : 'denied',
      sensitivity: message.scope.sensitivity, detail: message.delivery.error || message.id });
    hub.emit('message', message);
    return message;
  }
}

module.exports = Communication;
