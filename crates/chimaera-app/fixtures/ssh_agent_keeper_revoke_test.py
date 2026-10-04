"""Pure C2 runner cleanup regressions: no binary, VM, SSH or credentials."""
import os
import pathlib
import time
import threading
import types
import unittest
from unittest.mock import patch
import ssh_agent_keeper_retention as fixture


class RevokeOwnerTests(unittest.TestCase):
    def case(self, tail=b"C2_LINK_REVOKED\n", failure=None, cleanup_failure=None):
        owner = object.__new__(fixture.RetentionKeeper)
        owner.binary, owner.env = pathlib.Path("/fixed-fixture-not-executed"), {}
        owner.deadline = time.monotonic() + 30
        owner.end = lambda seconds: min(owner.deadline, time.monotonic() + seconds)
        gateway = types.SimpleNamespace(lock=threading.Lock(), total=0, origin="http://127.0.0.1:1")
        read, write = os.pipe()
        os.write(write, b"C2_LINK_OPENED\n")
        os.close(write)
        observer = types.SimpleNamespace(stdout=os.fdopen(read, "rb", buffering=0), returncode=0)
        commands, stopped = [], []
        owner.spawn = lambda *_: observer
        def control(_process, command, expected):
            commands.append(command)
            if command == b"REVOKE_HELD\n" and failure is not None:
                raise failure
        owner.control = control
        def stop(process):
            stopped.append(process)
            if cleanup_failure is not None:
                raise cleanup_failure
        try:
            with patch.object(fixture, "capture", return_value=tail), patch.object(fixture, "stop_owned", side_effect=stop):
                owner.revoke_case(gateway, object())
        finally:
            observer.stdout.close()
            self.assertEqual(commands, [b"REVOKE_ARM\n", b"REVOKE_HELD\n"])
            self.assertEqual(stopped, [observer])

    def test_exact_positive_suffix_and_positive_owner_cleanup(self):
        self.case()
        for tail in (b"", b"C2_LINK_FAILED\n", b"C2_LINK_REVOKED\nextra\n"):
            with self.assertRaises(fixture.Refused):
                self.case(tail=tail)

    def test_original_revocation_failure_survives_cleanup_failure(self):
        original = fixture.Refused("fixed-original-failure")
        with self.assertRaises(fixture.Refused) as error:
            self.case(failure=original, cleanup_failure=fixture.Refused("fixed-cleanup-failure"))
        self.assertIs(error.exception, original)

    def test_cleanup_failure_refuses_an_otherwise_positive_case(self):
        with self.assertRaises(fixture.Refused):
            self.case(cleanup_failure=fixture.Refused("fixed-cleanup-failure"))


class RevokeAllocationParserTests(unittest.TestCase):
    def positive(self, category='running'):
        return {'type': 'c2_revoked', 'authority_revoked': True, 'journal_nonterminal': 2,
                'batch_scheduler_running': True, 'attached_scheduler': category,
                'attached_pty_absent': True, 'forwards_absent': True, 'masters_absent': True,
                'hop_helpers_absent': True, 'agent_paths_absent': True, 'account_held': True, 'submissions': 2}

    def test_positive_requires_original_unresolved_journal_and_closed_interactive_cause(self):
        for category in ('running', 'cancelled-hup', 'cancelled-term'):
            self.assertEqual(fixture.revoke_positive(self.positive(category)), category)
        for category in ('cancelled', 'cancelled-stop', 'cancelled-unattributed', 'other', 'CANCELLED', True, None):
            self.assertIsNone(fixture.revoke_positive(self.positive(category)))
        original = self.positive()
        for key in original:
            bad = dict(original)
            del bad[key]
            self.assertIsNone(fixture.revoke_positive(bad))
            bad = dict(original)
            bad[key] = 'unknown'
            self.assertIsNone(fixture.revoke_positive(bad))
        for key in ('account_held', 'batch_scheduler_running', 'attached_pty_absent'):
            bad = dict(original); bad[key] = 1
            self.assertIsNone(fixture.revoke_positive(bad))
        bad = dict(original); bad['journal_nonterminal'] = True
        self.assertIsNone(fixture.revoke_positive(bad))
        bad = dict(original); bad['extra'] = 'never printed'
        self.assertIsNone(fixture.revoke_positive(bad))

    def test_allocation_refusals_project_only_exact_closed_verified_categories(self):
        predicates = ('not-observed', 'passed', 'journal-changed', 'journal-cardinality', 'hold-revision',
                      'hold-state', 'scheduler-cardinality', 'job-identity', 'job-phase', 'job-attached',
                      'job-scheduler-id', 'scheduler-identity', 'batch-state', 'attached-state', 'invalid')
        batches = ('not-observed', 'running', 'cancelled', 'timeout', 'stale-cause', 'other', 'invalid')
        attached = ('not-observed', 'running', 'cancelled-hup', 'cancelled-term', 'cancelled-stop',
                    'cancelled-unattributed', 'timeout', 'stale-cause', 'other', 'invalid')
        for predicate in predicates:
            for batch in batches:
                for interactive in attached:
                    allocation = {'predicate': predicate, 'batch': batch, 'attached': interactive}
                    value = {'type': 'c2_revoke_refused', 'stage': 'allocation-hold', 'timeout': False,
                             'allocation': allocation}
                    self.assertEqual(fixture.revoke_refusal(value),
                                     {'stage': 'allocation-hold', 'timeout': False, 'allocation': allocation})
                    self.assertIsNone(fixture.revoke_positive(value))
        original = {'type': 'c2_revoke_refused', 'stage': 'allocation-hold', 'timeout': False,
                    'allocation': {'predicate': 'attached-state', 'batch': 'running', 'attached': 'cancelled-stop'}}
        for key in ('predicate', 'batch', 'attached'):
            for raw in ('CANCELLED', '7002', 'raw arbitrary body', None, True, 2, {}, []):
                value = dict(original); value['allocation'] = dict(original['allocation']); value['allocation'][key] = raw
                self.assertIsNone(fixture.revoke_refusal(value))
            value = dict(original); value['allocation'] = dict(original['allocation']); del value['allocation'][key]
            self.assertIsNone(fixture.revoke_refusal(value))
        value = dict(original); value['allocation'] = dict(original['allocation'], raw='never printed')
        self.assertIsNone(fixture.revoke_refusal(value))
        value = dict(original); value['stage'] = 'authority'
        self.assertIsNone(fixture.revoke_refusal(value))
        value = dict(original); del value['allocation']
        self.assertIsNone(fixture.revoke_refusal(value))
        self.assertEqual(fixture.revoke_refusal({'type': 'c2_revoke_refused', 'stage': 'authority', 'timeout': True}),
                         {'stage': 'authority', 'timeout': True})

    def test_original_pidfd_diagnostic_accepts_only_passed_allocation_and_closed_classes(self):
        classes = ('not-observed', 'waiting', 'exited', 'descriptor-error', 'registration-error',
                   'readiness-error', 'poll-error', 'not-exited')
        for category in ('running', 'cancelled-hup', 'cancelled-term'):
            for stage in classes:
                for timeout in (False, True):
                    value = {'type': 'c2_revoke_refused', 'stage': 'pty-absence', 'timeout': timeout,
                             'pidfd': stage, 'allocation': {'predicate': 'passed', 'batch': 'running', 'attached': category}}
                    expected = dict(value); del expected['type']
                    self.assertEqual(fixture.revoke_refusal(value), expected)
                    self.assertIsNone(fixture.revoke_positive(value))
        original = {'type': 'c2_revoke_refused', 'stage': 'pty-absence', 'timeout': True,
                    'pidfd': 'waiting', 'allocation': {'predicate': 'passed', 'batch': 'running', 'attached': 'cancelled-hup'}}
        for key in original:
            bad = dict(original); del bad[key]
            self.assertIsNone(fixture.revoke_refusal(bad))
        for raw in ('invalid', '7002', 'raw untrusted value', None, True, 3, {}, []):
            bad = dict(original, pidfd=raw)
            self.assertIsNone(fixture.revoke_refusal(bad))
        for key, rejected in (('predicate', ('not-observed', 'hold-state', 'invalid')),
                              ('batch', ('not-observed', 'cancelled', 'timeout', 'invalid')),
                              ('attached', ('not-observed', 'cancelled-stop', 'timeout', 'invalid'))):
            for raw in rejected:
                bad = dict(original); bad['allocation'] = dict(original['allocation']); bad['allocation'][key] = raw
                self.assertIsNone(fixture.revoke_refusal(bad))
        bad = dict(original, extra='never printed')
        self.assertIsNone(fixture.revoke_refusal(bad))
        bad = dict(original); bad['allocation'] = dict(original['allocation'], extra='never printed')
        self.assertIsNone(fixture.revoke_refusal(bad))
        bad = dict(original, stage='allocation-hold')
        self.assertIsNone(fixture.revoke_refusal(bad))
        bad = dict(original, stage='auth-absence')
        self.assertIsNone(fixture.revoke_refusal(bad))


if __name__ == "__main__":
    unittest.main()
