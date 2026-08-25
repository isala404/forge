import asyncio
import unittest
from dataclasses import dataclass

from openfeature.evaluation_context import EvaluationContext
from openfeature.exception import ErrorCode
from openfeature.flag_evaluation import Reason

from forgelib.openfeature import ForgeProvider, telemetry_hook


@dataclass
class Details:
    value_json: str
    value_type: str
    variant: str | None
    reason: str
    error_code: str | None


class FakeForge:
    def __init__(self, details: Details) -> None:
        self.details = details
        self.calls: list[tuple[str, str, str | None]] = []

    async def flag_details(self, key: str, default_json: str, targeting_key: str | None) -> Details:
        self.calls.append((key, default_json, targeting_key))
        return self.details


class ForgeProviderTests(unittest.TestCase):
    def test_preserves_typed_details_and_context(self) -> None:
        forge = FakeForge(Details('"dark"', "string", "theme-v1", "static", None))
        provider = ForgeProvider(forge)
        context = EvaluationContext(targeting_key="user-1", attributes={"tenant": "acme"})
        detail = asyncio.run(provider.resolve_string_details_async("theme", "light", context))
        self.assertEqual(detail.value, "dark")
        self.assertEqual(detail.variant, "theme-v1")
        self.assertEqual(detail.reason, Reason.STATIC)
        self.assertEqual(forge.calls, [("theme", '"light"', "user-1")])
        self.assertEqual(context.attributes, {"tenant": "acme"})
        self.assertEqual(provider.get_provider_hooks(), [])
        self.assertIsNotNone(telemetry_hook())

    def test_returns_standard_missing_and_type_errors(self) -> None:
        missing = ForgeProvider(FakeForge(Details("false", "boolean", None, "default_missing", None)))
        detail = asyncio.run(missing.resolve_boolean_details_async("missing", False))
        self.assertEqual(detail.error_code, ErrorCode.FLAG_NOT_FOUND)
        self.assertEqual(detail.reason, Reason.ERROR)

        mismatch = ForgeProvider(FakeForge(Details('"wrong"', "string", None, "static", None)))
        detail = asyncio.run(mismatch.resolve_boolean_details_async("flag", False))
        self.assertIs(detail.value, False)
        self.assertEqual(detail.error_code, ErrorCode.TYPE_MISMATCH)


if __name__ == "__main__":
    unittest.main()
