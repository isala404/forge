from __future__ import annotations

import strawberry
from strawberry.tools import merge_types

from .auth import AuthMutation, AuthQuery
from .chat import ChatMutation, ChatQuery
from .message import MessageMutation, MessageQuery, MessageSubscription
from .ops import OpsMutation, OpsQuery
from .presence import PresenceMutation, PresenceQuery, PresenceSubscription
from .receipt import ReceiptMutation, ReceiptSubscription

Query = merge_types("Query", (AuthQuery, ChatQuery, MessageQuery, PresenceQuery, OpsQuery))
Mutation = merge_types(
    "Mutation",
    (AuthMutation, ChatMutation, MessageMutation, PresenceMutation, ReceiptMutation, OpsMutation),
)
Subscription = merge_types(
    "Subscription", (MessageSubscription, PresenceSubscription, ReceiptSubscription)
)

schema = strawberry.Schema(query=Query, mutation=Mutation, subscription=Subscription)
