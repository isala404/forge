import asyncio
import unittest

from native_memory_testing import main


class NativeMemoryTests(unittest.TestCase):
    def test_installed_memory_contract(self) -> None:
        asyncio.run(main())


if __name__ == "__main__":
    unittest.main()
