from __future__ import annotations

import asyncio
import json
from collections.abc import Mapping, Sequence
from typing import Any, Callable

from openfeature.contrib.hook.opentelemetry import TracingHook
from openfeature.evaluation_context import EvaluationContext
from openfeature.exception import ErrorCode
from openfeature.flag_evaluation import FlagResolutionDetails, Reason
from openfeature.provider import AbstractProvider, Metadata


_REASONS = {
    "static": Reason.STATIC,
    "percent_in": Reason.SPLIT,
    "percent_out": Reason.SPLIT,
    "targeting_match": Reason.TARGETING_MATCH,
    "targeting_miss": Reason.TARGETING_MATCH,
    "default_error": Reason.ERROR,
    "default_closed": Reason.ERROR,
}


class ForgeProvider(AbstractProvider):
    """Official OpenFeature provider over an initialized async Forge client.

    Evaluation context stays invocation-local. The provider installs no hooks or global state.
    """

    def __init__(self, forge: Any) -> None:
        super().__init__()
        if forge is None or not callable(getattr(forge, "flag_details", None)):
            raise TypeError("forge must be an initialized ForgeClient")
        self._forge = forge

    def get_metadata(self) -> Metadata:
        return Metadata(name="forge")

    def resolve_boolean_details(self, flag_key: str, default_value: bool, evaluation_context: EvaluationContext | None = None) -> FlagResolutionDetails[bool]:
        return self._resolve_sync(flag_key, default_value, evaluation_context, lambda value: type(value) is bool)

    async def resolve_boolean_details_async(self, flag_key: str, default_value: bool, evaluation_context: EvaluationContext | None = None) -> FlagResolutionDetails[bool]:
        return await self._resolve(flag_key, default_value, evaluation_context, lambda value: type(value) is bool)

    def resolve_string_details(self, flag_key: str, default_value: str, evaluation_context: EvaluationContext | None = None) -> FlagResolutionDetails[str]:
        return self._resolve_sync(flag_key, default_value, evaluation_context, lambda value: isinstance(value, str))

    async def resolve_string_details_async(self, flag_key: str, default_value: str, evaluation_context: EvaluationContext | None = None) -> FlagResolutionDetails[str]:
        return await self._resolve(flag_key, default_value, evaluation_context, lambda value: isinstance(value, str))

    def resolve_integer_details(self, flag_key: str, default_value: int, evaluation_context: EvaluationContext | None = None) -> FlagResolutionDetails[int]:
        return self._resolve_sync(flag_key, default_value, evaluation_context, lambda value: type(value) is int)

    async def resolve_integer_details_async(self, flag_key: str, default_value: int, evaluation_context: EvaluationContext | None = None) -> FlagResolutionDetails[int]:
        return await self._resolve(flag_key, default_value, evaluation_context, lambda value: type(value) is int)

    def resolve_float_details(self, flag_key: str, default_value: float, evaluation_context: EvaluationContext | None = None) -> FlagResolutionDetails[float]:
        return self._resolve_sync(flag_key, default_value, evaluation_context, lambda value: type(value) is float)

    async def resolve_float_details_async(self, flag_key: str, default_value: float, evaluation_context: EvaluationContext | None = None) -> FlagResolutionDetails[float]:
        return await self._resolve(flag_key, default_value, evaluation_context, lambda value: type(value) is float)

    def resolve_object_details(self, flag_key: str, default_value: Sequence[Any] | Mapping[str, Any], evaluation_context: EvaluationContext | None = None) -> FlagResolutionDetails[Sequence[Any] | Mapping[str, Any]]:
        return self._resolve_sync(flag_key, default_value, evaluation_context, lambda value: isinstance(value, (list, dict)))

    async def resolve_object_details_async(self, flag_key: str, default_value: Sequence[Any] | Mapping[str, Any], evaluation_context: EvaluationContext | None = None) -> FlagResolutionDetails[Sequence[Any] | Mapping[str, Any]]:
        return await self._resolve(flag_key, default_value, evaluation_context, lambda value: isinstance(value, (list, dict)))

    def _resolve_sync(self, flag_key: str, default_value: Any, evaluation_context: EvaluationContext | None, accepts: Callable[[Any], bool]) -> FlagResolutionDetails[Any]:
        try:
            asyncio.get_running_loop()
        except RuntimeError:
            return asyncio.run(self._resolve(flag_key, default_value, evaluation_context, accepts))
        return _failure(default_value, ErrorCode.PROVIDER_NOT_READY, "use the OpenFeature async client inside an event loop")

    async def _resolve(self, flag_key: str, default_value: Any, evaluation_context: EvaluationContext | None, accepts: Callable[[Any], bool]) -> FlagResolutionDetails[Any]:
        context = evaluation_context or EvaluationContext()
        try:
            details = await self._forge.flag_details(flag_key, json.dumps(default_value, separators=(",", ":")), context.targeting_key)
            value = json.loads(details.value_json)
        except Exception:
            return _failure(default_value, ErrorCode.GENERAL, "Forge evaluation failed")
        if not accepts(value):
            return _failure(default_value, ErrorCode.TYPE_MISMATCH, "flag value has the wrong type")
        if details.error_code:
            return _failure(value, ErrorCode.GENERAL, "Forge evaluation failed", details.variant)
        if details.reason == "default_missing":
            return _failure(value, ErrorCode.FLAG_NOT_FOUND, "flag was not found")
        return FlagResolutionDetails(value=value, reason=_REASONS.get(details.reason, Reason.DEFAULT), variant=details.variant)


def _failure(value: Any, code: ErrorCode, message: str, variant: str | None = None) -> FlagResolutionDetails[Any]:
    return FlagResolutionDetails(value=value, reason=Reason.ERROR, error_code=code, error_message=message, variant=variant)


def telemetry_hook() -> TracingHook:
    """Construct the official OTel feature_flag.evaluation hook without registering it."""

    return TracingHook()


__all__ = ["ForgeProvider", "telemetry_hook"]
