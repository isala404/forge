import json
import unittest
from pathlib import Path

import forgelib


VECTORS = json.loads(
    (Path(__file__).resolve().parents[3] / "contract" / "interop-vectors.json").read_text()
)
SCOPE = json.loads(
    (Path(__file__).resolve().parents[3] / "contract" / "scope-vectors.json").read_text()
)["valid"]


class InteropTests(unittest.TestCase):
    def test_scoped_names_are_reversible(self) -> None:
        args = tuple(SCOPE[name] for name in ("application", "tenant", "user", "resource"))
        self.assertEqual(forgelib.scope_kv_key(*args), SCOPE["kv"])
        self.assertEqual(forgelib.scope_blob_key(*args), SCOPE["blob"])
        self.assertEqual(
            forgelib.parse_scoped_name(SCOPE["topic"]),
            {
                "kind": "topic",
                "application": SCOPE["application"],
                "tenant": SCOPE["tenant"],
                "user": SCOPE["user"],
                "resource": SCOPE["resource"],
            },
        )
        with self.assertRaisesRegex(forgelib.InvalidError, "length") as invalid:
            forgelib.parse_scoped_name("v1|kv|+7:billing3:a:b3:u/19:invoice:7")
        self.assertEqual(invalid.exception.code, "Invalid")
        self.assertFalse(invalid.exception.retryable)
        with self.assertRaises(forgelib.InvalidError):
            forgelib.scope_topic("app", "", "user", "resource")
        with self.assertRaises(forgelib.LimitError):
            forgelib.scope_kv_key(*(value * 100 for value in ("a", "t", "u", "r")))

    def test_cloud_event_vector_round_trips(self) -> None:
        event = forgelib.decode_cloud_event(json.dumps(VECTORS["cloud_event"]["input"]))
        self.assertEqual(event["data"].hex(), VECTORS["cloud_event"]["data_hex"])
        self.assertEqual(event["extensions"], VECTORS["cloud_event"]["extensions"])
        self.assertEqual(
            forgelib.decode_cloud_event(forgelib.encode_cloud_event(event)), event
        )

    def test_environment_vector_and_conflict(self) -> None:
        environment = VECTORS["environment"]
        imported = forgelib.import_env_config(
            environment["source"], environment["mappings"]
        )
        self.assertEqual(imported, environment["imported"])
        self.assertEqual(
            forgelib.export_env_config(imported, environment["mappings"]),
            environment["exported"],
        )
        with self.assertRaisesRegex(ValueError, "conflict"):
            forgelib.import_env_config(
                {"DATABASE_URL": "one", "POSTGRES_URL": "two"},
                environment["mappings"],
            )


if __name__ == "__main__":
    unittest.main()
