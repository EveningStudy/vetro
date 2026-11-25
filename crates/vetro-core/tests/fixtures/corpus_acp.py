import json
import pathlib
import sys


path = pathlib.Path(sys.argv[1])
data = json.loads(path.read_bytes()) if path.is_file() else {}


def emit(message):
    print(json.dumps({"jsonrpc": "2.0", **message}), flush=True)


for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request.get("method")
    if method == "initialize":
        result = {
            "protocolVersion": 1,
            "agentCapabilities": {
                "loadSession": True,
                "sessionCapabilities": {"list": {}, "close": {}},
            },
        }
    elif method == "session/list":
        result = {"sessions": data.get("sessions", [])}
    elif method == "session/load":
        session = request["params"]["sessionId"]
        updates = data.get("updates", {}).get(session)
        if updates is None:
            emit({"id": request["id"], "error": {"code": -32602, "message": "No recorded replay"}})
            continue
        for update in updates:
            emit({"method": "session/update", "params": update})
        result = {}
    elif method == "session/close":
        result = {}
    else:
        emit({"id": request["id"], "error": {"code": -32601, "message": "Offline replay only"}})
        continue
    emit({"id": request["id"], "result": result})
