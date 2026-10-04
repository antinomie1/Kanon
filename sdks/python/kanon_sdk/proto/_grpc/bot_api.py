# Generated service slice; see tools/split-protocol-bindings.py.
"""Generated bot api bindings."""
import grpc
from .. import plugin_pb2 as plugin__pb2

class BotApiServiceStub:
    """3. 核心 API 服务 (运行在 Core 端，监听 core.sock / loopback，Host 主动连接调用)
    """

    def __init__(self, channel):
        """Constructor.

        Args:
            channel: A grpc.Channel.
        """
        self.RegisterHost = channel.unary_unary(
                '/kanon.plugin.v1.BotApiService/RegisterHost',
                request_serializer=plugin__pb2.RegisterHostRequest.SerializeToString,
                response_deserializer=plugin__pb2.RegisterHostResponse.FromString,
                _registered_method=True)
        self.IngestEvent = channel.unary_unary(
                '/kanon.plugin.v1.BotApiService/IngestEvent',
                request_serializer=plugin__pb2.IngestEventRequest.SerializeToString,
                response_deserializer=plugin__pb2.IngestEventResponse.FromString,
                _registered_method=True)
        self.SendMessage = channel.unary_unary(
                '/kanon.plugin.v1.BotApiService/SendMessage',
                request_serializer=plugin__pb2.SendMessageRequest.SerializeToString,
                response_deserializer=plugin__pb2.SendMessageResponse.FromString,
                _registered_method=True)
        self.ReplyMessage = channel.unary_unary(
                '/kanon.plugin.v1.BotApiService/ReplyMessage',
                request_serializer=plugin__pb2.DeliverMessageRequest.SerializeToString,
                response_deserializer=plugin__pb2.DeliverMessageResponse.FromString,
                _registered_method=True)
        self.RequestLLM = channel.unary_stream(
                '/kanon.plugin.v1.BotApiService/RequestLLM',
                request_serializer=plugin__pb2.LLMRequest.SerializeToString,
                response_deserializer=plugin__pb2.LLMChunk.FromString,
                _registered_method=True)
        self.CallPlatformApi = channel.unary_unary(
                '/kanon.plugin.v1.BotApiService/CallPlatformApi',
                request_serializer=plugin__pb2.PlatformApiRequest.SerializeToString,
                response_deserializer=plugin__pb2.PlatformApiResponse.FromString,
                _registered_method=True)
        self.SetStorage = channel.unary_unary(
                '/kanon.plugin.v1.BotApiService/SetStorage',
                request_serializer=plugin__pb2.SetStorageRequest.SerializeToString,
                response_deserializer=plugin__pb2.SetStorageResponse.FromString,
                _registered_method=True)
        self.GetStorage = channel.unary_unary(
                '/kanon.plugin.v1.BotApiService/GetStorage',
                request_serializer=plugin__pb2.GetStorageRequest.SerializeToString,
                response_deserializer=plugin__pb2.GetStorageResponse.FromString,
                _registered_method=True)
        self.DeleteStorage = channel.unary_unary(
                '/kanon.plugin.v1.BotApiService/DeleteStorage',
                request_serializer=plugin__pb2.DeleteStorageRequest.SerializeToString,
                response_deserializer=plugin__pb2.DeleteStorageResponse.FromString,
                _registered_method=True)
        self.ListStorage = channel.unary_unary(
                '/kanon.plugin.v1.BotApiService/ListStorage',
                request_serializer=plugin__pb2.ListStorageRequest.SerializeToString,
                response_deserializer=plugin__pb2.ListStorageResponse.FromString,
                _registered_method=True)
        self.Ping = channel.unary_unary(
                '/kanon.plugin.v1.BotApiService/Ping',
                request_serializer=plugin__pb2.PingRequest.SerializeToString,
                response_deserializer=plugin__pb2.PingResponse.FromString,
                _registered_method=True)
        self.GetConversationHistory = channel.unary_unary(
                '/kanon.plugin.v1.BotApiService/GetConversationHistory',
                request_serializer=plugin__pb2.ConversationHistoryRequest.SerializeToString,
                response_deserializer=plugin__pb2.ConversationHistoryResponse.FromString,
                _registered_method=True)
        self.ListConversations = channel.unary_unary(
                '/kanon.plugin.v1.BotApiService/ListConversations',
                request_serializer=plugin__pb2.ConversationsRequest.SerializeToString,
                response_deserializer=plugin__pb2.ConversationList.FromString,
                _registered_method=True)
        self.NewConversation = channel.unary_unary(
                '/kanon.plugin.v1.BotApiService/NewConversation',
                request_serializer=plugin__pb2.ConversationsRequest.SerializeToString,
                response_deserializer=plugin__pb2.ConversationList.FromString,
                _registered_method=True)
        self.SwitchConversation = channel.unary_unary(
                '/kanon.plugin.v1.BotApiService/SwitchConversation',
                request_serializer=plugin__pb2.SelectConversationRequest.SerializeToString,
                response_deserializer=plugin__pb2.ConversationList.FromString,
                _registered_method=True)
        self.DeleteConversation = channel.unary_unary(
                '/kanon.plugin.v1.BotApiService/DeleteConversation',
                request_serializer=plugin__pb2.SelectConversationRequest.SerializeToString,
                response_deserializer=plugin__pb2.ConversationList.FromString,
                _registered_method=True)
        self.AppendConversation = channel.unary_unary(
                '/kanon.plugin.v1.BotApiService/AppendConversation',
                request_serializer=plugin__pb2.AppendConversationRequest.SerializeToString,
                response_deserializer=plugin__pb2.AppendConversationResponse.FromString,
                _registered_method=True)
        self.ListPersonas = channel.unary_unary(
                '/kanon.plugin.v1.BotApiService/ListPersonas',
                request_serializer=plugin__pb2.ListPersonasRequest.SerializeToString,
                response_deserializer=plugin__pb2.ListPersonasResponse.FromString,
                _registered_method=True)
        self.UpsertPersona = channel.unary_unary(
                '/kanon.plugin.v1.BotApiService/UpsertPersona',
                request_serializer=plugin__pb2.Persona.SerializeToString,
                response_deserializer=plugin__pb2.UpsertPersonaResponse.FromString,
                _registered_method=True)
        self.DeletePersona = channel.unary_unary(
                '/kanon.plugin.v1.BotApiService/DeletePersona',
                request_serializer=plugin__pb2.DeletePersonaRequest.SerializeToString,
                response_deserializer=plugin__pb2.DeletePersonaResponse.FromString,
                _registered_method=True)
        self.RunAgent = channel.unary_unary(
                '/kanon.plugin.v1.BotApiService/RunAgent',
                request_serializer=plugin__pb2.RunAgentRequest.SerializeToString,
                response_deserializer=plugin__pb2.RunAgentResponse.FromString,
                _registered_method=True)
        self.RefreshPluginMeta = channel.unary_unary(
                '/kanon.plugin.v1.BotApiService/RefreshPluginMeta',
                request_serializer=plugin__pb2.RefreshPluginMetaRequest.SerializeToString,
                response_deserializer=plugin__pb2.RefreshPluginMetaResponse.FromString,
                _registered_method=True)
        self.RenderImage = channel.unary_unary(
                '/kanon.plugin.v1.BotApiService/RenderImage',
                request_serializer=plugin__pb2.RenderImageRequest.SerializeToString,
                response_deserializer=plugin__pb2.RenderImageResponse.FromString,
                _registered_method=True)


class BotApiServiceServicer:
    """3. 核心 API 服务 (运行在 Core 端，监听 core.sock / loopback，Host 主动连接调用)
    """

    def RegisterHost(self, request, context):
        """Missing associated documentation comment in .proto file."""
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def IngestEvent(self, request, context):
        """Missing associated documentation comment in .proto file."""
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def SendMessage(self, request, context):
        """Missing associated documentation comment in .proto file."""
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def ReplyMessage(self, request, context):
        """Reply to an original platform event through the same outbound FIFO. Success
        means the platform adapter accepted delivery, not merely queue admission.
        A deadline/disconnect is an unknown outcome and must not trigger an automatic retry.
        """
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def RequestLLM(self, request, context):
        """Missing associated documentation comment in .proto file."""
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def CallPlatformApi(self, request, context):
        """Calls one action of a built-in adapter's platform API (OneBot action, Milky endpoint) and
        returns its result. Errors are gRPC statuses: NOT_FOUND for an unknown platform,
        UNIMPLEMENTED when the adapter offers no API, INVALID_ARGUMENT for a malformed action,
        UNAVAILABLE when the platform rejected or could not take the call.
        """
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def SetStorage(self, request, context):
        """Central key-value store, one namespace per plugin, persisted by the core in `data/kv.db`.
        Values are opaque bytes (the SDKs store JSON). A key that expired reads as absent.
        """
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def GetStorage(self, request, context):
        """Missing associated documentation comment in .proto file."""
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def DeleteStorage(self, request, context):
        """Missing associated documentation comment in .proto file."""
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def ListStorage(self, request, context):
        """Lists the plugin's live keys starting with `prefix`, sorted.
        """
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def Ping(self, request, context):
        """Liveness probe answered by the core itself, without touching the pipeline.

        Hosts use it to notice that the core they registered with has exited: a host whose core is
        gone must stop, otherwise it keeps serving its platform (and, for adapters, keeps the
        platform's long-lived connection) while no bot instance can answer — which shows up as a
        ghost bot double-handling messages.
        """
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def GetConversationHistory(self, request, context):
        """Reads the model conversation an inbound message belongs to. Read-only: history is
        append-only and only the core writes it. NOT_FOUND when no enabled instance claims the
        platform, UNAVAILABLE when no model is configured, INVALID_ARGUMENT without a context.
        """
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def ListConversations(self, request, context):
        """--- Conversations: the sessions of one chat, as `/ls`, `/new`, `/switch` and `/del` see them.
        Every call names the chat by an inbound message (`context`), resolved exactly as when the
        core answers that message. NOT_FOUND when no enabled instance claims it.
        """
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def NewConversation(self, request, context):
        """Starts a new, empty conversation and makes it current (what `/new` does).
        """
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def SwitchConversation(self, request, context):
        """Makes another conversation of the same chat current. NOT_FOUND for an unknown session.
        """
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def DeleteConversation(self, request, context):
        """Deletes a conversation's history and records. Deleting the current one leaves the chat on a
        new, empty conversation. NOT_FOUND for an unknown session.
        """
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def AppendConversation(self, request, context):
        """Appends finished turns to the current conversation, e.g. a command exchange the model should
        remember. Messages must alternate user/assistant, starting with user and ending with
        assistant; anything else is INVALID_ARGUMENT. Append-only: nothing already stored changes.
        """
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def ListPersonas(self, request, context):
        """--- Personas: the operator's persona catalog (`data/personas.json`).
        """
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def UpsertPersona(self, request, context):
        """Creates or replaces a persona. INVALID_ARGUMENT for an empty id or prompt, FAILED_PRECONDITION
        for the built-in read-only persona.
        """
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def DeletePersona(self, request, context):
        """Missing associated documentation comment in .proto file."""
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def RunAgent(self, request, context):
        """Runs the node's agent (model + tool loop) for a plugin and returns its final answer. Unlike
        `RequestLLM` it can use tools and can run inside a chat's conversation. UNAVAILABLE when no
        model is configured, NOT_FOUND when `context` names a chat no enabled instance claims.
        """
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def RefreshPluginMeta(self, request, context):
        """Asks the core to fetch this host's `GetPluginMeta` again, so tools, commands and triggers a
        plugin added or removed at runtime take effect for the next turn. NOT_FOUND for an unknown
        host.
        """
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')

    def RenderImage(self, request, context):
        """Renders text (or SVG) into a PNG in the plugin's data directory, for sending as an image.
        INVALID_ARGUMENT for empty input or malformed SVG; UNAVAILABLE when no usable font exists.
        """
        context.set_code(grpc.StatusCode.UNIMPLEMENTED)
        context.set_details('Method not implemented!')
        raise NotImplementedError('Method not implemented!')


def add_BotApiServiceServicer_to_server(servicer, server):
    rpc_method_handlers = {
            'RegisterHost': grpc.unary_unary_rpc_method_handler(
                    servicer.RegisterHost,
                    request_deserializer=plugin__pb2.RegisterHostRequest.FromString,
                    response_serializer=plugin__pb2.RegisterHostResponse.SerializeToString,
            ),
            'IngestEvent': grpc.unary_unary_rpc_method_handler(
                    servicer.IngestEvent,
                    request_deserializer=plugin__pb2.IngestEventRequest.FromString,
                    response_serializer=plugin__pb2.IngestEventResponse.SerializeToString,
            ),
            'SendMessage': grpc.unary_unary_rpc_method_handler(
                    servicer.SendMessage,
                    request_deserializer=plugin__pb2.SendMessageRequest.FromString,
                    response_serializer=plugin__pb2.SendMessageResponse.SerializeToString,
            ),
            'ReplyMessage': grpc.unary_unary_rpc_method_handler(
                    servicer.ReplyMessage,
                    request_deserializer=plugin__pb2.DeliverMessageRequest.FromString,
                    response_serializer=plugin__pb2.DeliverMessageResponse.SerializeToString,
            ),
            'RequestLLM': grpc.unary_stream_rpc_method_handler(
                    servicer.RequestLLM,
                    request_deserializer=plugin__pb2.LLMRequest.FromString,
                    response_serializer=plugin__pb2.LLMChunk.SerializeToString,
            ),
            'CallPlatformApi': grpc.unary_unary_rpc_method_handler(
                    servicer.CallPlatformApi,
                    request_deserializer=plugin__pb2.PlatformApiRequest.FromString,
                    response_serializer=plugin__pb2.PlatformApiResponse.SerializeToString,
            ),
            'SetStorage': grpc.unary_unary_rpc_method_handler(
                    servicer.SetStorage,
                    request_deserializer=plugin__pb2.SetStorageRequest.FromString,
                    response_serializer=plugin__pb2.SetStorageResponse.SerializeToString,
            ),
            'GetStorage': grpc.unary_unary_rpc_method_handler(
                    servicer.GetStorage,
                    request_deserializer=plugin__pb2.GetStorageRequest.FromString,
                    response_serializer=plugin__pb2.GetStorageResponse.SerializeToString,
            ),
            'DeleteStorage': grpc.unary_unary_rpc_method_handler(
                    servicer.DeleteStorage,
                    request_deserializer=plugin__pb2.DeleteStorageRequest.FromString,
                    response_serializer=plugin__pb2.DeleteStorageResponse.SerializeToString,
            ),
            'ListStorage': grpc.unary_unary_rpc_method_handler(
                    servicer.ListStorage,
                    request_deserializer=plugin__pb2.ListStorageRequest.FromString,
                    response_serializer=plugin__pb2.ListStorageResponse.SerializeToString,
            ),
            'Ping': grpc.unary_unary_rpc_method_handler(
                    servicer.Ping,
                    request_deserializer=plugin__pb2.PingRequest.FromString,
                    response_serializer=plugin__pb2.PingResponse.SerializeToString,
            ),
            'GetConversationHistory': grpc.unary_unary_rpc_method_handler(
                    servicer.GetConversationHistory,
                    request_deserializer=plugin__pb2.ConversationHistoryRequest.FromString,
                    response_serializer=plugin__pb2.ConversationHistoryResponse.SerializeToString,
            ),
            'ListConversations': grpc.unary_unary_rpc_method_handler(
                    servicer.ListConversations,
                    request_deserializer=plugin__pb2.ConversationsRequest.FromString,
                    response_serializer=plugin__pb2.ConversationList.SerializeToString,
            ),
            'NewConversation': grpc.unary_unary_rpc_method_handler(
                    servicer.NewConversation,
                    request_deserializer=plugin__pb2.ConversationsRequest.FromString,
                    response_serializer=plugin__pb2.ConversationList.SerializeToString,
            ),
            'SwitchConversation': grpc.unary_unary_rpc_method_handler(
                    servicer.SwitchConversation,
                    request_deserializer=plugin__pb2.SelectConversationRequest.FromString,
                    response_serializer=plugin__pb2.ConversationList.SerializeToString,
            ),
            'DeleteConversation': grpc.unary_unary_rpc_method_handler(
                    servicer.DeleteConversation,
                    request_deserializer=plugin__pb2.SelectConversationRequest.FromString,
                    response_serializer=plugin__pb2.ConversationList.SerializeToString,
            ),
            'AppendConversation': grpc.unary_unary_rpc_method_handler(
                    servicer.AppendConversation,
                    request_deserializer=plugin__pb2.AppendConversationRequest.FromString,
                    response_serializer=plugin__pb2.AppendConversationResponse.SerializeToString,
            ),
            'ListPersonas': grpc.unary_unary_rpc_method_handler(
                    servicer.ListPersonas,
                    request_deserializer=plugin__pb2.ListPersonasRequest.FromString,
                    response_serializer=plugin__pb2.ListPersonasResponse.SerializeToString,
            ),
            'UpsertPersona': grpc.unary_unary_rpc_method_handler(
                    servicer.UpsertPersona,
                    request_deserializer=plugin__pb2.Persona.FromString,
                    response_serializer=plugin__pb2.UpsertPersonaResponse.SerializeToString,
            ),
            'DeletePersona': grpc.unary_unary_rpc_method_handler(
                    servicer.DeletePersona,
                    request_deserializer=plugin__pb2.DeletePersonaRequest.FromString,
                    response_serializer=plugin__pb2.DeletePersonaResponse.SerializeToString,
            ),
            'RunAgent': grpc.unary_unary_rpc_method_handler(
                    servicer.RunAgent,
                    request_deserializer=plugin__pb2.RunAgentRequest.FromString,
                    response_serializer=plugin__pb2.RunAgentResponse.SerializeToString,
            ),
            'RefreshPluginMeta': grpc.unary_unary_rpc_method_handler(
                    servicer.RefreshPluginMeta,
                    request_deserializer=plugin__pb2.RefreshPluginMetaRequest.FromString,
                    response_serializer=plugin__pb2.RefreshPluginMetaResponse.SerializeToString,
            ),
            'RenderImage': grpc.unary_unary_rpc_method_handler(
                    servicer.RenderImage,
                    request_deserializer=plugin__pb2.RenderImageRequest.FromString,
                    response_serializer=plugin__pb2.RenderImageResponse.SerializeToString,
            ),
    }
    generic_handler = grpc.method_handlers_generic_handler(
            'kanon.plugin.v1.BotApiService', rpc_method_handlers)
    server.add_generic_rpc_handlers((generic_handler,))
    server.add_registered_method_handlers('kanon.plugin.v1.BotApiService', rpc_method_handlers)


 # This class is part of an EXPERIMENTAL API.
