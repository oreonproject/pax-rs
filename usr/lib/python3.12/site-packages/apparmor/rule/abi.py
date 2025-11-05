# ----------------------------------------------------------------------
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

from apparmor.common import AppArmorBug
from apparmor.regex import RE_ABI
from apparmor.rule.include import IncludeRule, IncludeRuleset
from apparmor.translations import init_translation

_ = init_translation()


# abi and include rules have a very similar syntax
# base AbiRule on IncludeRule to inherit most of its behaviour
class AbiRule(IncludeRule):
    """Class to handle and store a single abi rule"""

    rule_name = 'abi'

    def __init__(self, path, ifexists, ismagic, audit=False, deny=False, allow_keyword=False,
                 comment='', log_event=None):

        super().__init__(path, ifexists, ismagic,
                         audit=audit, deny=deny, allow_keyword=allow_keyword,
                         comment=comment, log_event=log_event)

        # abi doesn't support 'if exists'
        if ifexists:
            raise AppArmorBug('Attempt to use %s rule with if exists flag' % self.__class__.__name__)

    @classmethod
    def _match(cls, raw_rule):
        return RE_ABI.search(raw_rule)

    def get_clean(self, depth=0):
        """return rule (in clean/default formatting)"""

        space = '  ' * depth

        if self.ismagic:
            return ('%s%s <%s>,%s' % (space, self.rule_name, self.path, self.comment))
        else:
            return ('%s%s "%s",%s' % (space, self.rule_name, self.path, self.comment))

    def logprof_header_localvars(self):
        return [_('Abi'), self.get_clean()]


class AbiRuleset(IncludeRuleset):
    """Class to handle and store a collection of abi rules"""
