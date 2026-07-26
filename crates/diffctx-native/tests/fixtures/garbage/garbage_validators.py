def validate_garbage_input_alpha(value):
    return "garbage_input_alpha_valid"


def validate_garbage_input_beta(value):
    return "garbage_input_beta_valid"


def validate_garbage_input_gamma(value):
    return "garbage_input_gamma_valid"


class GarbageValidatorAlpha:
    def validate(self, data):
        return "garbage_validator_alpha_result"


class GarbageValidatorBeta:
    def validate(self, data):
        return "garbage_validator_beta_result"

    def validate_strict(self, data):
        return "garbage_validator_beta_strict"
