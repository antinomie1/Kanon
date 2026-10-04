# Generated service slice; see tools/split-protocol-bindings.py.
"""Generated pipeline bindings."""
import grpc
from .. import plugin_pb2 as plugin__pb2

class MessagePipelineServiceStub:
    """2. 消息与事件管道服务 (运行在 Host 端，Core 主动连接各 Host 端点发起调用)
    """

    def __init__(self, channel):
        """Constructor.

        Args:
            channel: A grpc.Channel.
        """
        self.OnPreFilter = channel.unary_unary(
                '/kanon.plugin.v1.MessagePipelineService/OnPreFilter',
                request_serializer=plugin__pb2.PipelineEventRequest.SerializeToString,
                response_deserializer=plugin__pb2.PreFilterResult.FromString,
                _registered_method=True)
        self.OnExecuteCommand = channel.unary_unary(
                '/kanon.plugin.v1.MessagePipelineService/OnExecuteCommand',
                request_serializer=plugin__pb2.CommandExecuteRequest.SerializeToString,
                response_deserializer=plugin__pb2.CommandExecuteResponse.FromString,
                _registered_method=True)
        self.OnCallTool = channel.unary_unary(
                '/kanon.plugin.v1.MessagePipelineService/OnCallTool',
                request_serializer=plugin__pb2.ToolCallRequest.SerializeToString,
                response_deserializer=plugin__pb2.ToolCallResponse.FromString,
                _registered_method=True)
        self.OnEvent = channel.unary_unary(
                '/kanon.plugin.v1.MessagePipelineService/OnEvent',
                request_serializer=plugin__pb2.EventNotification.SerializeToString,
                response_deserializer=plugin__pb2.EventAck.FromString,
                _registered_method=True)
        self.OnDeliverMessage = channel.unary_unary(
                '/kanon.plugin.v1.MessagePipelineService/OnDeliverMessage',
                request_serializer=plugin__pb2.DeliverMessageRequest.SerializeToString,
                response_deserializer=plugin__pb2.DeliverMessageResponse.FromString,
                _registered_method=True)
        self.OnDecorateReply = channel.unary_unary(
                '/kanon.plugin.v1.MessagePipelineService/OnDecorateReply',
                request_serializer=plugin__pb2.DecorateReplyRequest.SerializeToString,
                response_deserializer=plugin__pb2.DecorateReplyResult.FromString,
                _registered_method=True)
        self.OnPrepareTurn = channel.unary_unary(
                '/kanon.plugin.v1.MessagePipelineService/OnPrepareTurn',
                request_serializer=plugin__pb2.PrepareTurnRequest.SerializeToString,
                response_deserializer=plugin__pb2.PrepareTurnResult.FromString,
                _registered_method=True)
        self.OnLlmRequest = channel.unary_unary(
                '/kanon.plugin.v1.MessagePipelineService/OnLlmRequest',
                request_serializer=plugin__pb2.LlmRequestHookRequest.SerializeToString,
                response_deserializer=plugin__pb2.LlmRequestHookResult.FromString,
                _registered_method=True)
        self.OnHttpRequest = channel.unary_unary(
                '/kanon.plugin.v1.MessagePipelineService/OnHttpRequest',
                request_serializer=plugin__pb2.HttpRequest.SerializeToString,
                response_deserializer=plugin__pb2.HttpResponse.FromString,
                _registered_method=True)


class MessagePipelineServiceServicer:
    """2. 消息与事件管道服务 (运行在 Host 端，Core 主动连接各 Host 端点发起调用)
    """

    def OnPreFilter(self, request, context):
        """Missing associated documentation comment in .proto file."""
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def OnExecuteCommand(self, request, context):
        """Missing associated documentation comment in .proto file."""
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def OnCallTool(self, request, context):
        """Missing associated documentation comment in .proto file."""
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def OnEvent(self, request, context):
        """Fire-and-forget lifecycle events, sent only for the kinds a plugin lists in `PluginMeta.events`.
        """
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def OnDeliverMessage(self, request, context):
        """Missing associated documentation comment in .proto file."""
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def OnDecorateReply(self, request, context):
        """Rewrites a reply before it is delivered; called only for plugins with
        `PluginMeta.decorates_replies`, in host priority order, each seeing the previous result.
        """
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def OnPrepareTurn(self, request, context):
        """Adds plugin context to the current turn just before the model answers it; called only for
        plugins with `PluginMeta.prepares_turns`. The text is prepended to the current user message,
        never to the system prompt or the history, so the cached request prefix stays stable.
        """
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def OnLlmRequest(self, request, context):
        """Rewrites the system prompt of a model conversation; called only for plugins with
        `PluginMeta.rewrites_system_prompt`, in host priority order, each seeing the previous result.
        Called once per turn before the first model request (the result is reused for the turn's
        tool rounds and its compaction), and never for a plugin's own `RequestLLM`/`RunAgent` calls
        unless they run inside the conversation.

        The system prompt is the first thing in every request, so its bytes decide the provider's
        prompt cache: a rewrite must be deterministic for a given conversation (no clocks, counters
        or per-message data — those belong in `OnPrepareTurn`). A failing or slow plugin (deadline
        as `OnPrepareTurn`) leaves the prompt as it was.
        """
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def OnHttpRequest(self, request, context):
        """Serves an HTTP request addressed to the plugin's web routes; called only for plugins with
        `PluginMeta.serves_http`. The management gateway forwards
        `/api/v1/plugins/<plugin_id>/http/<path>` here; the plugin routes on `path` itself.
        """
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')


def add_MessagePipelineServiceServicer_to_server(servicer, server):
    rpc_method_handlers = {
            'OnPreFilter': grpc.unary_unary_rpc_method_handler(
                    servicer.OnPreFilter,
                    request_deserializer=plugin__pb2.PipelineEventRequest.FromString,
                    response_serializer=plugin__pb2.PreFilterResult.SerializeToString,
            ),
            'OnExecuteCommand': grpc.unary_unary_rpc_method_handler(
                    servicer.OnExecuteCommand,
                    request_deserializer=plugin__pb2.CommandExecuteRequest.FromString,
                    response_serializer=plugin__pb2.CommandExecuteResponse.SerializeToString,
            ),
            'OnCallTool': grpc.unary_unary_rpc_method_handler(
                    servicer.OnCallTool,
                    request_deserializer=plugin__pb2.ToolCallRequest.FromString,
                    response_serializer=plugin__pb2.ToolCallResponse.SerializeToString,
            ),
            'OnEvent': grpc.unary_unary_rpc_method_handler(
                    servicer.OnEvent,
                    request_deserializer=plugin__pb2.EventNotification.FromString,
                    response_serializer=plugin__pb2.EventAck.SerializeToString,
            ),
            'OnDeliverMessage': grpc.unary_unary_rpc_method_handler(
                    servicer.OnDeliverMessage,
                    request_deserializer=plugin__pb2.DeliverMessageRequest.FromString,
                    response_serializer=plugin__pb2.DeliverMessageResponse.SerializeToString,
            ),
            'OnDecorateReply': grpc.unary_unary_rpc_method_handler(
                    servicer.OnDecorateReply,
                    request_deserializer=plugin__pb2.DecorateReplyRequest.FromString,
                    response_serializer=plugin__pb2.DecorateReplyResult.SerializeToString,
            ),
            'OnPrepareTurn': grpc.unary_unary_rpc_method_handler(
                    servicer.OnPrepareTurn,
                    request_deserializer=plugin__pb2.PrepareTurnRequest.FromString,
                    response_serializer=plugin__pb2.PrepareTurnResult.SerializeToString,
            ),
            'OnLlmRequest': grpc.unary_unary_rpc_method_handler(
                    servicer.OnLlmRequest,
                    request_deserializer=plugin__pb2.LlmRequestHookRequest.FromString,
                    response_serializer=plugin__pb2.LlmRequestHookResult.SerializeToString,
            ),
            'OnHttpRequest': grpc.unary_unary_rpc_method_handler(
                    servicer.OnHttpRequest,
                    request_deserializer=plugin__pb2.HttpRequest.FromString,
                    response_serializer=plugin__pb2.HttpResponse.SerializeToString,
            ),
    }
    generic_handler = grpc.method_handlers_generic_handler(
            'kanon.plugin.v1.MessagePipelineService', rpc_method_handlers)
    server.add_generic_rpc_handlers((generic_handler,))
    server.add_registered_method_handlers('kanon.plugin.v1.MessagePipelineService', rpc_method_handlers)


 # This class is part of an EXPERIMENTAL API.
class MessagePipelineService:
    """2. 消息与事件管道服务 (运行在 Host 端，Core 主动连接各 Host 端点发起调用)
    """

    @staticmethod
    def OnPreFilter(request,
            target,
            options=(),
            channel_credentials=None,
            call_credentials=None,
            insecure=False,
            compression=None,
            wait_for_ready=None,
            timeout=None,
            metadata=None):
        return grpc.experimental.unary_unary(
            request,
            target,
            '/kanon.plugin.v1.MessagePipelineService/OnPreFilter',
            plugin__pb2.PipelineEventRequest.SerializeToString,
            plugin__pb2.PreFilterResult.FromString,
            options,
            channel_credentials,
            insecure,
            call_credentials,
            compression,
            wait_for_ready,
            timeout,
            metadata,
            _registered_method=True)

    @staticmethod
    def OnExecuteCommand(request,
            target,
            options=(),
            channel_credentials=None,
            call_credentials=None,
            insecure=False,
            compression=None,
            wait_for_ready=None,
            timeout=None,
            metadata=None):
        return grpc.experimental.unary_unary(
            request,
            target,
            '/kanon.plugin.v1.MessagePipelineService/OnExecuteCommand',
            plugin__pb2.CommandExecuteRequest.SerializeToString,
            plugin__pb2.CommandExecuteResponse.FromString,
            options,
            channel_credentials,
            insecure,
            call_credentials,
            compression,
            wait_for_ready,
            timeout,
            metadata,
            _registered_method=True)

    @staticmethod
    def OnCallTool(request,
            target,
            options=(),
            channel_credentials=None,
            call_credentials=None,
            insecure=False,
            compression=None,
            wait_for_ready=None,
            timeout=None,
            metadata=None):
        return grpc.experimental.unary_unary(
            request,
            target,
            '/kanon.plugin.v1.MessagePipelineService/OnCallTool',
            plugin__pb2.ToolCallRequest.SerializeToString,
            plugin__pb2.ToolCallResponse.FromString,
            options,
            channel_credentials,
            insecure,
            call_credentials,
            compression,
            wait_for_ready,
            timeout,
            metadata,
            _registered_method=True)

    @staticmethod
    def OnEvent(request,
            target,
            options=(),
            channel_credentials=None,
            call_credentials=None,
            insecure=False,
            compression=None,
            wait_for_ready=None,
            timeout=None,
            metadata=None):
        return grpc.experimental.unary_unary(
            request,
            target,
            '/kanon.plugin.v1.MessagePipelineService/OnEvent',
            plugin__pb2.EventNotification.SerializeToString,
            plugin__pb2.EventAck.FromString,
            options,
            channel_credentials,
            insecure,
            call_credentials,
            compression,
            wait_for_ready,
            timeout,
            metadata,
            _registered_method=True)

    @staticmethod
    def OnDeliverMessage(request,
            target,
            options=(),
            channel_credentials=None,
            call_credentials=None,
            insecure=False,
            compression=None,
            wait_for_ready=None,
            timeout=None,
            metadata=None):
        return grpc.experimental.unary_unary(
            request,
            target,
            '/kanon.plugin.v1.MessagePipelineService/OnDeliverMessage',
            plugin__pb2.DeliverMessageRequest.SerializeToString,
            plugin__pb2.DeliverMessageResponse.FromString,
            options,
            channel_credentials,
            insecure,
            call_credentials,
            compression,
            wait_for_ready,
            timeout,
            metadata,
            _registered_method=True)

    @staticmethod
    def OnDecorateReply(request,
            target,
            options=(),
            channel_credentials=None,
            call_credentials=None,
            insecure=False,
            compression=None,
            wait_for_ready=None,
            timeout=None,
            metadata=None):
        return grpc.experimental.unary_unary(
            request,
            target,
            '/kanon.plugin.v1.MessagePipelineService/OnDecorateReply',
            plugin__pb2.DecorateReplyRequest.SerializeToString,
            plugin__pb2.DecorateReplyResult.FromString,
            options,
            channel_credentials,
            insecure,
            call_credentials,
            compression,
            wait_for_ready,
            timeout,
            metadata,
            _registered_method=True)

    @staticmethod
    def OnPrepareTurn(request,
            target,
            options=(),
            channel_credentials=None,
            call_credentials=None,
            insecure=False,
            compression=None,
            wait_for_ready=None,
            timeout=None,
            metadata=None):
        return grpc.experimental.unary_unary(
            request,
            target,
            '/kanon.plugin.v1.MessagePipelineService/OnPrepareTurn',
            plugin__pb2.PrepareTurnRequest.SerializeToString,
            plugin__pb2.PrepareTurnResult.FromString,
            options,
            channel_credentials,
            insecure,
            call_credentials,
            compression,
            wait_for_ready,
            timeout,
            metadata,
            _registered_method=True)

    @staticmethod
    def OnLlmRequest(request,
            target,
            options=(),
            channel_credentials=None,
            call_credentials=None,
            insecure=False,
            compression=None,
            wait_for_ready=None,
            timeout=None,
            metadata=None):
        return grpc.experimental.unary_unary(
            request,
            target,
            '/kanon.plugin.v1.MessagePipelineService/OnLlmRequest',
            plugin__pb2.LlmRequestHookRequest.SerializeToString,
            plugin__pb2.LlmRequestHookResult.FromString,
            options,
            channel_credentials,
            insecure,
            call_credentials,
            compression,
            wait_for_ready,
            timeout,
            metadata,
            _registered_method=True)

    @staticmethod
    def OnHttpRequest(request,
            target,
            options=(),
            channel_credentials=None,
            call_credentials=None,
            insecure=False,
            compression=None,
            wait_for_ready=None,
            timeout=None,
            metadata=None):
        return grpc.experimental.unary_unary(
            request,
            target,
            '/kanon.plugin.v1.MessagePipelineService/OnHttpRequest',
            plugin__pb2.HttpRequest.SerializeToString,
            plugin__pb2.HttpResponse.FromString,
            options,
            channel_credentials,
            insecure,
            call_credentials,
            compression,
            wait_for_ready,
            timeout,
            metadata,
            _registered_method=True)
