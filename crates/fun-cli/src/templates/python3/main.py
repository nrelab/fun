import base64
import json
import os
import sys


def handler(request):
    payload = json.loads(request["body"].decode("utf-8") or "{}")
    name = payload.get("name", "world")
    greeting = os.environ.get("GREETING", "Hello")
    return {
        "status": 200,
        "headers": [["content-type", "application/json"]],
        "body": json.dumps({"message": f"{greeting}, {name}!"}).encode("utf-8"),
    }


def main():
    for line in sys.stdin:
        request_id = ""
        try:
            request = json.loads(line)
            request_id = request["request_id"]
            if request["version"] != 1:
                raise ValueError("unsupported protocol version")
            request["body"] = base64.b64decode(request["body"], validate=True)
            result = handler(request)
            frame = {
                "version": 1,
                "request_id": request_id,
                "outcome": "response",
                "status": result["status"],
                "headers": result.get("headers", []),
                "body": base64.b64encode(result.get("body", b"")).decode("ascii"),
            }
        except Exception as error:
            frame = {
                "version": 1,
                "request_id": request_id,
                "outcome": "error",
                "error": {"kind": "user", "message": str(error)},
            }
        sys.stdout.write(json.dumps(frame, separators=(",", ":")) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
