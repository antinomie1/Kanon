# Generated service slice; see tools/split-protocol-bindings.py.
"""Generated host bindings."""
import grpc
from .. import plugin_pb2 as plugin__pb2

class PluginHostServiceStub:
    """1. 插件宿主生命周期服务 (运行在 Host 端)
    """

    def __init__(self, channel):
        """Constructor.

        Args:
            channel: A grpc.Channel.
        """
        self.Ping = channel.unary_unary(
                '/kanon.plugin.v1.PluginHostService/Ping',
                request_serializer=plugin__pb2.PingRequest.SerializeToString,
                response_deserializer=plugin__pb2.PingResponse.FromString,
                _registered_method=True)
        self.ReloadPluginConfig = channel.unary_unary(
                '/kanon.plugin.v1.PluginHostService/ReloadPluginConfig',
                request_serializer=plugin__pb2.ReloadPluginConfigRequest.SerializeToString,
                response_deserializer=plugin__pb2.ReloadPluginConfigResponse.FromString,
                _registered_method=True)
        self.GetPluginMeta = channel.unary_unary(
                '/kanon.plugin.v1.PluginHostService/GetPluginMeta',
                request_serializer=plugin__pb2.GetPluginMetaRequest.SerializeToString,
                response_deserializer=plugin__pb2.GetPluginMetaResponse.FromString,
                _registered_method=True)
        self.InvokeAction = channel.unary_unary(
                '/kanon.plugin.v1.PluginHostService/InvokeAction',
                request_serializer=plugin__pb2.PluginActionRequest.SerializeToString,
                response_deserializer=plugin__pb2.PluginActionResponse.FromString,
                _registered_method=True)


class PluginHostServiceServicer:
    """1. 插件宿主生命周期服务 (运行在 Host 端)
    """

    def Ping(self, request, context):
        """Missing associated documentation comment in .proto file."""
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def ReloadPluginConfig(self, request, context):
        """Missing associated documentation comment in .proto file."""
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def GetPluginMeta(self, request, context):
        """Missing associated documentation comment in .proto file."""
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def InvokeAction(self, request, context):
        """Management action invoked by the control plane.

        Actions are the console counterpart of tools: credential binding, QR login and diagnostics
        are operations an operator triggers, not functions the model may call. Adapter plugins must
        expose such operations here so that their tool list stays empty — an adapter's tools would
        otherwise be offered to the LLM, which then tries to bind credentials mid-conversation.
        """
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')


def add_PluginHostServiceServicer_to_server(servicer, server):
    rpc_method_handlers = {
            'Ping': grpc.unary_unary_rpc_method_handler(
                    servicer.Ping,
                    request_deserializer=plugin__pb2.PingRequest.FromString,
                    response_serializer=plugin__pb2.PingResponse.SerializeToString,
            ),
            'ReloadPluginConfig': grpc.unary_unary_rpc_method_handler(
                    servicer.ReloadPluginConfig,
                    request_deserializer=plugin__pb2.ReloadPluginConfigRequest.FromString,
                    response_serializer=plugin__pb2.ReloadPluginConfigResponse.SerializeToString,
            ),
            'GetPluginMeta': grpc.unary_unary_rpc_method_handler(
                    servicer.GetPluginMeta,
                    request_deserializer=plugin__pb2.GetPluginMetaRequest.FromString,
                    response_serializer=plugin__pb2.GetPluginMetaResponse.SerializeToString,
            ),
            'InvokeAction': grpc.unary_unary_rpc_method_handler(
                    servicer.InvokeAction,
                    request_deserializer=plugin__pb2.PluginActionRequest.FromString,
                    response_serializer=plugin__pb2.PluginActionResponse.SerializeToString,
            ),
    }
    generic_handler = grpc.method_handlers_generic_handler(
            'kanon.plugin.v1.PluginHostService', rpc_method_handlers)
    server.add_generic_rpc_handlers((generic_handler,))
    server.add_registered_method_handlers('kanon.plugin.v1.PluginHostService', rpc_method_handlers)


 # This class is part of an EXPERIMENTAL API.
class PluginHostService:
    """1. 插件宿主生命周期服务 (运行在 Host 端)
    """

    @staticmethod
    def Ping(request,
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
            '/kanon.plugin.v1.PluginHostService/Ping',
            plugin__pb2.PingRequest.SerializeToString,
            plugin__pb2.PingResponse.FromString,
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
    def ReloadPluginConfig(request,
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
            '/kanon.plugin.v1.PluginHostService/ReloadPluginConfig',
            plugin__pb2.ReloadPluginConfigRequest.SerializeToString,
            plugin__pb2.ReloadPluginConfigResponse.FromString,
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
    def GetPluginMeta(request,
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
            '/kanon.plugin.v1.PluginHostService/GetPluginMeta',
            plugin__pb2.GetPluginMetaRequest.SerializeToString,
            plugin__pb2.GetPluginMetaResponse.FromString,
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
    def InvokeAction(request,
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
            '/kanon.plugin.v1.PluginHostService/InvokeAction',
            plugin__pb2.PluginActionRequest.SerializeToString,
            plugin__pb2.PluginActionResponse.FromString,
            options,
            channel_credentials,
            insecure,
            call_credentials,
            compression,
            wait_for_ready,
            timeout,
            metadata,
            _registered_method=True)
