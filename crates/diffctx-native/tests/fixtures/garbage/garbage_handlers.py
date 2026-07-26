class GarbageHandlerAlpha:
    def handle_garbage_event_alpha(self, event):
        return "garbage_event_alpha_handled"

    def process_garbage_request_alpha(self, request):
        return "garbage_request_alpha_processed"


class GarbageHandlerBeta:
    def handle_garbage_event_beta(self, event):
        return "garbage_event_beta_handled"

    def process_garbage_request_beta(self, request):
        return "garbage_request_beta_processed"


class GarbageHandlerGamma:
    def handle_garbage_event_gamma(self, event):
        return "garbage_event_gamma_handled"


def garbage_event_dispatcher(event_type):
    return f"garbage_dispatched_{event_type}"


def garbage_request_router(path):
    return f"garbage_routed_{path}"
