"""Parser backend adapters (PRD T4 item 2).

Each module adapts one engine family to the stable ``ParserBackend`` protocol.
Adapters take an injected client callable, so importing this package pulls in
no heavy dependencies and unit tests need no network/services.
"""
