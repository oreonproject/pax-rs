# ----------------------------------------------------------------------
#    Copyright (C) 2013 Kshitij Gupta <kgupta8592@gmail.com>
#    Copyright (C) 2020 Christian Boltz <apparmor@cboltz.de>
#
#    This program is free software; you can redistribute it and/or
#    modify it under the terms of version 2 of the GNU General Public
#    License as published by the Free Software Foundation.
#
#    This program is distributed in the hope that it will be useful,
#    but WITHOUT ANY WARRANTY; without even the implied warranty of
#    MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
#    GNU General Public License for more details.
#
# ----------------------------------------------------------------------

import re

from apparmor.common import AppArmorBug, AppArmorException
from apparmor.regex import RE_PROFILE_VARIABLE, strip_quotes
from apparmor.rule import BaseRule, BaseRuleset, parse_comment, quote_if_needed
from apparmor.translations import init_translation

_ = init_translation()


class VariableRule(BaseRule):
    """Class to handle and store a single variable rule"""

    rule_name = 'variable'

    def __init__(self, varname, mode, values, audit=False, deny=False, allow_keyword=False,
                 comment='', log_event=None):

        super().__init__(audit=audit, deny=deny, allow_keyword=allow_keyword,
                         comment=comment, log_event=log_event)

        # variables don't support audit or deny
        if audit:
            raise AppArmorBug('Attempt to initialize %s with audit flag' % self.__class__.__name__)
        if deny:
            raise AppArmorBug('Attempt to initialize %s with deny flag' % self.__class__.__name__)

        if type(varname) is not str:
            raise AppArmorBug('Passed unknown type for varname to %s: %s' % (self.__class__.__name__, varname))
        if not varname.startswith('@{'):
            raise AppArmorException("Passed invalid varname to %s (doesn't start with '@{'): %s" % (self.__class__.__name__, varname))
        if not varname.endswith('}'):
            raise AppArmorException("Passed invalid varname to %s (doesn't end with '}'): %s" % (self.__class__.__name__, varname))

        if type(mode) is not str:
            raise AppArmorBug('Passed unknown type for variable assignment mode to %s: %s' % (self.__class__.__name__, mode))
        if mode not in ('=', '+='):
            raise AppArmorBug('Passed unknown variable assignment mode to %s: %s' % (self.__class__.__name__, mode))

        if type(values) is not set:
            raise AppArmorBug('Passed unknown type for values to %s: %s' % (self.__class__.__name__, values))
        if not values:
            raise AppArmorException('Passed empty list of values to %s: %s' % (self.__class__.__name__, values))

        self.varname = varname
        self.mode = mode
        self.values = values

    @classmethod
    def _match(cls, raw_rule):
        return RE_PROFILE_VARIABLE.search(raw_rule)

    @classmethod
    def _parse(cls, raw_rule):
        """parse raw_rule and return VariableRule"""

        matches = cls._match(raw_rule)
        if not matches:
            raise AppArmorException(_("Invalid variable rule '%s'") % raw_rule)

        comment = parse_comment(matches)

        varname = matches.group('varname')
        mode = matches.group('mode')
        values = separate_vars(matches.group('values'))

        return VariableRule(varname, mode, values,
                            audit=False, deny=False, allow_keyword=False, comment=comment)

    def get_clean(self, depth=0):
        """return rule (in clean/default formatting)"""

        space = '  ' * depth

        data = []
        for value in sorted(self.values):
            if not value:
                value = '""'
            data.append(quote_if_needed(value))

        return '%s%s %s %s' % (space, self.varname, self.mode, ' '.join(data))

    def is_covered_localvars(self, other_rule):
        """check if other_rule is covered by this rule object"""

        if self.varname != other_rule.varname:
            return False

        if self.mode != other_rule.mode:
            return False

        if not self._is_covered_list(self.values, None, set(other_rule.values), None, 'values'):
            return False

        # still here? -> then it is covered
        return True

    def is_equal_localvars(self, rule_obj, strict):
        """compare if rule-specific variables are equal"""

        if not type(rule_obj) == VariableRule:
            raise AppArmorBug('Passed non-variable rule: %s' % str(rule_obj))

        if self.varname != rule_obj.varname:
            return False

        if self.mode != rule_obj.mode:
            return False

        if self.values != rule_obj.values:
            return False

        return True

    def logprof_header_localvars(self):
        headers = []

        return headers + [
            _('Variable'), self.get_clean(),
        ]


class VariableRuleset(BaseRuleset):
    """Class to handle and store a collection of variable rules"""

    def add(self, rule, cleanup=False):
        """Add variable rule object

           If the variable name is already known, raise an exception because re-defining a variable isn't allowed.
        """

        if rule.mode == '=':
            for knownrule in self.rules:
                if rule.varname == knownrule.varname:
                    raise AppArmorException(
                        _('Redefining existing variable %(variable)s: %(value)s')
                        % {'variable': rule.varname, 'value': rule.values})

        super().add(rule, cleanup)

    def get_merged_variables(self):
        """Get merged variables of this VariableRuleset.

           Note that no error checking is done because variables can be defined in one file and extended in another.
        """

        var_set = {}
        var_add = {}

        for rule in self.rules:
            if rule.mode == '=':
                var_set[rule.varname] = rule.values  # blindly set, add() prevents redefinition of variables
            else:
                if not var_add.get(rule.varname):
                    var_add[rule.varname] = rule.values
                else:
                    var_add[rule.varname] |= rule.values

        return {'=': var_set, '+=': var_add}


def separate_vars(vs):
    """Returns a list of all the values for a variable"""
    data = set()
    vs = vs.strip()

    re_vars = re.compile(r'^(("[^"]*")|([^"\s]+))\s*(.*)$')
    while re_vars.search(vs):
        matches = re_vars.search(vs).groups()

        if matches[0].endswith(','):
            raise AppArmorException(_('Variable declarations do not accept trailing commas'))

        data.add(strip_quotes(matches[0]))
        vs = matches[3].strip()

    if vs:
        raise AppArmorException('Variable assignments contains invalid parts (unbalanced quotes?): %s' % vs)

    return data
