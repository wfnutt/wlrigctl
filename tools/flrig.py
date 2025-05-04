import xmlrpc.client

s = xmlrpc.client.ServerProxy("http://127.0.0.1:12345")  # adjust host:port if needed
print("server version:", s.main.get_version())
print("methods:", s.system.listMethods())

