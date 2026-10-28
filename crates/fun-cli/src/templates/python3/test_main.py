import json
import unittest
from unittest.mock import patch

from main import handler


class HandlerTest(unittest.TestCase):
    def test_handler_returns_rust_fun_response(self):
        request = {
            "body": json.dumps({"name": "Python"}).encode("utf-8"),
        }
        with patch.dict("os.environ", {"GREETING": "Welcome"}):
            response = handler(request)

        self.assertEqual(response["status"], 200)
        self.assertIn(["content-type", "application/json"], response["headers"])
        self.assertEqual(
            json.loads(response["body"]),
            {"message": "Welcome, Python!"},
        )


if __name__ == "__main__":
    unittest.main()
