def garbage_api_endpoint_alpha(request):
    return {"garbage_response": "alpha"}


def garbage_api_endpoint_beta(request):
    return {"garbage_response": "beta"}


def garbage_api_endpoint_gamma(request):
    return {"garbage_response": "gamma"}


def garbage_api_endpoint_delta(request):
    return {"garbage_response": "delta"}


class GarbageApiController:
    def garbage_get_all(self):
        return "garbage_api_get_all"

    def garbage_get_one(self, id):
        return f"garbage_api_get_{id}"

    def garbage_create(self, data):
        return "garbage_api_created"

    def garbage_update(self, id, data):
        return f"garbage_api_updated_{id}"

    def garbage_delete(self, id):
        return f"garbage_api_deleted_{id}"
