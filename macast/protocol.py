# Copyright (c) 2021 by xfangfang. All Rights Reserved.
import json
import os
import re
import sys
import time
import uuid
import secrets
import http.client
import logging
import cherrypy
import threading

from lxml import etree
from queue import Queue
from enum import Enum
from cherrypy import _cpnative_server

from .utils import load_xml, XMLPath, Setting, SettingProperty, cherrypy_publish, SETTING_DIR
from .discovery import advertisable_addresses

logger = logging.getLogger("Protocol")
logger.setLevel(logging.INFO)

SERVICE_STATE_OBSERVED = {
    "AVTransport": ['TransportState',
                    'TransportStatus',
                    'CurrentMediaDuration',
                    'CurrentTrackDuration',
                    'CurrentTrack',
                    'NumberOfTracks'],
    "RenderingControl": ['Volume', 'Mute'],
    "ConnectionManager": ['A_ARG_TYPE_Direction',
                          'SinkProtocolInfo',
                          'CurrentConnectionIDs']
}

SOAP_ENV_NS = 'http://schemas.xmlsoap.org/soap/envelope/'
SOAP_ENCODING_NS = 'http://schemas.xmlsoap.org/soap/encoding/'
UPNP_CONTROL_NS = 'urn:schemas-upnp-org:control-1-0'


def normalize_soap_body(rawbody):
    """Drop a leading BOM / indentation that precedes the XML declaration.

    XML requires the declaration to be the very first thing in the document,
    so expat rejects any leading whitespace with "XML declaration allowed only
    at the start of the document". Clients do send it anyway: BiliPai
    (Android, okhttp) pretty-prints the whole envelope and indents even the
    declaration by 12 spaces:

        b'            <?xml version="1.0" encoding="utf-8"?>\\n            <s:Envelope ...>'

    which made every SetAVTransportURI fail with HTTP 500 and the cast look
    like it silently did nothing. Leading whitespace carries no meaning here,
    so strip it (plus a UTF-8 BOM) instead of refusing the request.

    Accepts bytes (the usual path from the HTTP body) or str, and always
    returns bytes: lxml refuses a *str* that carries an encoding declaration,
    so a decoded body must never be handed back to it.
    """
    if isinstance(rawbody, str):
        rawbody = rawbody.encode('utf-8')
    return rawbody.lstrip(b'\xef\xbb\xbf \t\r\n')


def soap_fault(error_code, error_description, fault_code='s:Client'):
    """Serialise a UPnP SOAP Fault (control-1-0) as a response body.

    A DLNA control point can only interpret a SOAP Fault; the HTML page
    CherryPy renders for an unhandled exception is opaque to it, so a bad
    request ends up looking like "casting does nothing".
    """
    root = etree.Element(etree.QName(SOAP_ENV_NS, 'Envelope'),
                         nsmap={'s': SOAP_ENV_NS})
    root.attrib['{{{}}}encodingStyle'.format(SOAP_ENV_NS)] = SOAP_ENCODING_NS
    body = etree.SubElement(root, etree.QName(SOAP_ENV_NS, 'Body'))
    fault = etree.SubElement(body, etree.QName(SOAP_ENV_NS, 'Fault'))
    etree.SubElement(fault, 'faultcode').text = fault_code
    etree.SubElement(fault, 'faultstring').text = 'UPnPError'
    detail = etree.SubElement(fault, 'detail')
    error = etree.SubElement(detail, etree.QName(UPNP_CONTROL_NS, 'UPnPError'))
    etree.SubElement(error, 'errorCode').text = str(error_code)
    etree.SubElement(error, 'errorDescription').text = error_description
    return etree.tostring(root, encoding='UTF-8', xml_declaration=False)


class PlaybackGuard:
    """Transparent renderer wrapper that enforces one playback owner.

    With several protocols live, two senders can cast at the same time. The
    player itself resolves that by switching track, but nobody tells the
    displaced sender — its phone keeps showing a progress bar for something
    that stopped.

    Every protocol reaches the player through ``Protocol.renderer``, so this
    is the one place the handoff can be observed. All attribute access is
    forwarded unchanged; only ``set_media_url`` (start of playback) triggers
    ownership transfer.
    """

    #: owner of the current playback, or None
    owner = None

    def __init__(self, protocol, renderer):
        self._protocol = protocol
        self._renderer = renderer

    def set_media_url(self, *args, **kwargs):
        previous = PlaybackGuard.owner
        if previous is not None and previous is not self._protocol:
            logger.info("Playback taken over from %s by %s",
                        type(previous).__name__, type(self._protocol).__name__)
            try:
                previous.release_playback()
            except Exception as e:  # never block playback on a notification
                logger.error("release_playback failed on %s: %s",
                             type(previous).__name__, e)
        PlaybackGuard.owner = self._protocol
        return self._renderer.set_media_url(*args, **kwargs)

    def __getattr__(self, name):
        # Only called when normal lookup misses, so this never shadows a real
        # renderer attribute.
        return getattr(self._renderer, name)


class Protocol:
    def __init__(self):
        self._handler = None

    @property
    def handler(self):
        if self._handler is None:
            self._handler = Handler()
        return self._handler

    def start(self):
        pass

    def stop(self):
        pass

    def reload(self):
        self.stop()
        self.start()

    def methods(self):
        return list(filter(lambda m: m.startswith('set_state_') and callable(getattr(self, m)), dir(self)))

    @property
    def renderer(self):
        renderers = cherrypy.engine.publish('get_renderer')
        if len(renderers) == 0:
            logger.error("Unable to find an available renderer.")
            return None
        renderer = renderers.pop()
        if renderer is None:
            return None
        return PlaybackGuard(self, renderer)

    def release_playback(self):
        """Another protocol took over playback; release control.

        There is one player behind every protocol. When a second sender starts
        casting, mpv just switches track and the displaced sender's app keeps
        showing controls for a stream it no longer owns. The default does
        nothing; protocols able to signal STOP to their client can override
        this. The point is that the handoff is *offered* at a single choke
        point rather than being each protocol's problem.
        """
        pass

    # The following methods are called by the renderer to set the playback status within the protocol,
    # which will be passed to the client (generally the mobile phone)

    def set_state_position(self, data: str):
        """
        :param data: string, eg: 00:00:00
        :return:
        """
        pass

    def set_state_duration(self, data: str):
        """
        :param data: string, eg: 00:00:00
        :return:
        """
        pass

    def set_state_pause(self):
        """
        :return:
        """
        pass

    def set_state_play(self):
        """
        :return:
        """
        pass

    def set_state_stop(self):
        """
        :return:
        """
        pass

    def set_state_eof(self):
        """
        :return:
        """
        pass

    def set_state_transport(self, data: str):
        """
        :param data: string in [PLAYING, PAUSED_PLAYBACK, STOPPED, NO_MEDIA_PRESENT]
        :return:
        """
        pass

    def set_state_transport_error(self):
        """
        :return:
        """
        pass

    def set_state_mute(self, data: bool):
        """
        :param data: bool
        :return:
        """
        pass

    def set_state_volume(self, data: int):
        """
        :param data: int 0-100
        :return:
        """
        pass

    def set_state_speed(self, data: str):
        pass

    def set_state_display_subtitle(self, data: bool):
        """ set custom subtitle file path
        :param data: bool, whether display the subtitle
        :return:
        """
        pass

    def set_state_url(self, data: str):
        pass

    def set_state(self, state_name, state_value):
        pass

    def get_state(self, state_name):
        return ''

    def get_state_title(self) -> str:
        """
        :return: string, eg: demo
        """
        return ''

    def get_state_url(self) -> str:
        """
        :return: string, eg: http://10.10.10.10/demo.mp4
        """
        return ''

    def get_state_position(self) -> str:
        """
        :return: string, eg: 00:00:00
        """
        return '00:00:00'

    def get_state_duration(self) -> str:
        """
        :return: string, eg: 00:00:00
        """
        return '00:00:00'

    def get_state_volume(self) -> int:
        """
        :return: int, range from 0 to 100
        """
        return 80

    def get_state_mute(self) -> bool:
        """
        :return: bool
        """
        return False

    def get_state_transport_state(self) -> str:
        """
        :return: string in [PLAYING, PAUSED_PLAYBACK, STOPPED, NO_MEDIA_PRESENT]
        """
        return 'STOPPED'

    def get_state_transport_status(self) -> str:
        """
        :return: string in [OK, ERROR_OCCURRED]
        """
        return 'OK'

    def get_state_speed(self) -> str:
        return '1'

    def get_state_display_subtitle(self) -> bool:
        return True


class ObserveClient:
    def __init__(self, service, url, timeout=1800):
        self.url = url
        self.service = service
        self.startTime = int(time.time())
        self.sid = "uuid:{}".format(uuid.uuid4())
        self.timeout = timeout
        self.seq = 0
        self.host = re.findall(r"//([0-9:.]*)", url)[0]
        self.path = re.findall(r"//[0-9:.]*(.*)$", url)[0]
        print("-----------------------------", self.host)
        self.error = 0

    def is_timeout(self):
        return int(time.time()) - self.startTime > self.timeout

    def update(self, timeout=1800):
        self.startTime = int(time.time())
        self.timeout = timeout

    def send_event_callback(self, data):
        """Sending event data to client
        """
        headers = {"NT": "upnp:event",
                   "NTS": "upnp:propchange",
                   "CONTENT-TYPE": 'text/xml; charset="utf-8"',
                   "SERVER": Setting.get_server_info(),
                   "SID": self.sid,
                   "SEQ": self.seq,
                   "TIMEOUT": "Second-{}".format(self.timeout)
                   }
        namespace = 'urn:schemas-upnp-org:event-1-0'
        root = etree.Element(etree.QName(namespace, 'propertyset'),
                             nsmap={'e': namespace})
        if self.service == 'ConnectionManager':
            for i in data:
                prop = etree.SubElement(
                    root, '{urn:schemas-upnp-org:event-1-0}property')
                item = etree.SubElement(prop, i)
                item.text = str(data[i])
        else:
            prop = etree.SubElement(
                root, '{urn:schemas-upnp-org:event-1-0}property')
            last_change = etree.SubElement(prop, 'LastChange')
            event = etree.Element('Event')
            event.attrib['xmlns'] = 'urn:schemas-upnp-org:metadata-1-0/AVT/'
            instance_id = etree.SubElement(event, 'InstanceID')
            instance_id.set('val', '0')
            for i in data:
                p = etree.SubElement(instance_id, i)
                p.set('val', str(data[i]))
            last_change.text = etree.tostring(event, encoding="UTF-8").decode()
        data = etree.tostring(root, encoding="UTF-8")
        logger.debug("Prop Change---------")
        logger.debug(data)
        conn = http.client.HTTPConnection(self.host, timeout=5)
        conn.request("NOTIFY", self.path, data, headers)
        conn.close()
        self.seq = self.seq + 1


class DataType(Enum):
    boolean = 'boolean'
    i2 = 'i2'
    ui2 = 'ui2'
    i4 = 'i4'
    ui4 = 'ui4'
    string = 'string'


class StateVariable:
    """The state of render
    """

    def __init__(self, name, send_events, datatype, service):
        self.name = name
        self.sendEvents = True if send_events == 'yes' else False
        self.datatype = DataType(datatype)
        self.minimum = None
        self.maximum = None
        self.allowedValueList = None
        self.value = '' if self.datatype == DataType.string else 0
        self.service = service

    def set_value(self, value):
        self.value = value

    def set_allowed_value_list(self, values):
        self.allowedValueList = values
        if 'NOT_IMPLEMENTED' in values:
            self.value = 'NOT_IMPLEMENTED'

    def set_allowed_value_range(self, minimum, maximum):
        self.minimum = minimum
        self.maximum = maximum


class Argument:
    def __init__(self, name, state, value=None):
        self.name = name
        self.state = state
        self.value = value


class Action:
    """Operations supported by render.

    Parameters
    ----------
    name : string
        The name of the operation.
    input : list<Argument>
        A set of state values.
    output : list<Argument>
        A set of state values.

    """

    def __init__(self, name, input, output):
        self.name = name
        self.input = input
        self.output = output


class Service:
    service_map = {}

    @classmethod
    def get(cls, name):
        return cls.service_map.get(name, Service(name))

    @classmethod
    def build(cls, name, ns, actions):
        cls.service_map[name] = Service(name, ns, actions)

    def __init__(self, name, namespace='', actions={}):
        self.name = name
        self.namespace = namespace
        self.actions = actions


class DLNAProtocol(Protocol):

    def __init__(self):
        super(DLNAProtocol, self).__init__()
        self.running = False
        self.state_list = {}
        self.action_list = {}
        self.event_thread = None
        self.event_subscribes = {}  # subscribe devices
        self.state_queue = Queue()  # states needed be send to subscribe devices
        self.removed_device_queue = Queue()  # devices needed be removed
        self.append_device_queue = Queue()  # devices needed be added
        self._state_event = threading.Event()  # wakes the event thread immediately
        self._init_sem = threading.Semaphore(20)  # bound init-event threads
        self.playlist = []          # cast playlist (list of uris)
        self.current_index = -1     # current playlist position
        self.init_services()  # create services handle function from xml file
        self.init_state()  # set default value

    @property
    def handler(self):
        if self._handler is None:
            self._handler = DLNAHandler()
        return self._handler

    def init_state(self):
        self.set_state('CurrentPlayMode', 'NORMAL')
        self.set_state('TransportPlaySpeed', 1)
        self.set_state('TransportStatus', 'OK')
        self.set_state('RelativeCounterPosition', 2147483647)
        self.set_state('AbsoluteCounterPosition', 2147483647)
        self.set_state('A_ARG_TYPE_Direction', 'Output')
        self.set_state('CurrentConnectionIDs', '0')
        self.set_state('PlaybackStorageMedium', 'None')
        self.set_state('SinkProtocolInfo', load_xml(XMLPath.PROTOCOL_INFO.value).strip())

    def init_services(self, description=XMLPath.DESCRIPTION.value):
        """
        :param description: dlna description xml file
        :return:
        """
        desc = etree.parse(description).getroot()
        for service_type in desc.iter('{urn:schemas-upnp-org:device-1-0}serviceType'):
            # service_type:  urn:schemas-upnp-org:service:ConnectionManager:1
            namespace = service_type.text
            service = service_type.text.split(":")[3]
            self.build_action(namespace, service, etree.parse(
                XMLPath.BASE_PATH.value + f"/xml/{service}.xml").getroot())

    def build_action(self, namespace, service, xml):
        """
        :param namespace: eg: urn:schemas-upnp-org:service:ConnectionManager:1
        :param service:  eg: ConnectionManager
        :param xml: xml content of service
        :return:
        """
        """Build action and variable list from xml file
        """
        ns = '{urn:schemas-upnp-org:service-1-0}'
        # get state variable from xml file
        for state_variable in xml.iter(ns + 'stateVariable'):
            name = state_variable.find(ns + "name").text

            data = StateVariable(name,
                                 state_variable.attrib['sendEvents'],
                                 state_variable.find(ns + "dataType").text,
                                 service)
            default_value = state_variable.find(ns + "defaultValue")
            if default_value is not None:
                data.set_value(default_value.text)
            allowed_value_list = state_variable.find(ns + "allowedValueList")
            if allowed_value_list is not None:
                values = [
                    value.text
                    for value in allowed_value_list.findall(ns + "allowedValue")
                ]
                data.set_allowed_value_list(values)

            allowed_value_range = state_variable.find(ns + "allowedValueRange")
            if allowed_value_range is not None:
                data.set_allowed_value_range(
                    int(allowed_value_range.find(ns + "minimum").text),
                    int(allowed_value_range.find(ns + "maximum").text))
            self.state_list[name] = data

        # get action from xml file
        actions = {}
        for action in xml.iter(ns + 'action'):
            name = action.find(ns + "name").text
            input = []
            output = []
            argument_list = action.find(ns + "argumentList")
            if argument_list is not None:
                for argument in argument_list.findall(ns + 'argument'):
                    data = Argument(
                        argument.find(ns + "name").text,
                        argument.find(ns + "relatedStateVariable").text)
                    if argument.find(ns + "direction").text == 'in':
                        input.append(data)
                    else:
                        output.append(data)
            actions[name] = Action(name, input, output)
        # self.action_list[service] = actions
        Service.build(service, namespace, actions)

    def add_subscribe(self, service, url, timeout=1800):
        """Add a DLNA client to subscribe list
        """
        logger.error("SUBSCRIBE: " + url)
        for client in self.event_subscribes:
            if self.event_subscribes[client].url == url and \
                    self.event_subscribes[client].service == service:
                s = self.event_subscribes[client]
                s.update(timeout)
                logger.error("SUBSCRIBE UPDATE")
                return {
                    "SID": s.sid,
                    "TIMEOUT": "Second-{}".format(s.timeout)
                }
        logger.error("SUBSCRIBE ADD")
        client = ObserveClient(service, url, timeout)
        self.append_device_queue.put(client)
        threading.Thread(target=self._send_init_event,
                         args=(service, client),
                         daemon=True).start()
        return {
            "SID": client.sid,
            "TIMEOUT": "Second-{}".format(client.timeout)
        }

    def _send_init_event(self, service, client):
        """When there is a client subscription,
        the first event callback will send all the state values of the service.
        Bounded by a semaphore so a flood of SUBSCRIBEs cannot spawn
        unbounded threads."""
        with self._init_sem:
            self.send_init_event(service, client)

    def send_init_event(self, service, client):
        """When there is a client subscription,
        the first event callback will send all the state values of the service.
        """
        data = {}
        for state in SERVICE_STATE_OBSERVED[service]:
            data[state] = self.state_list[state].value
        try:
            client.send_event_callback(data)
        except Exception as e:
            logger.error(str(e))

    def remove_subscribe(self, sid):
        """Remove a DLNA client from subscribe list
        """
        if sid in self.event_subscribes:
            self.removed_device_queue.put(sid)
        return 200

    def renew_subscribe(self, sid, timeout=1800):
        """Renew a DLNA client in subcribe list
        """
        if sid in self.event_subscribes:
            self.event_subscribes[sid].update(timeout)
            return 200
        return 412

    def send_states_to_clients(self, state_change_list):
        """Sending the states in the stateChangeList to the clients which subscribe to them.
        :param state_change_list:
        :return:
        """
        if not bool(state_change_list):
            return
        # remove offline clients
        while not self.removed_device_queue.empty():
            sid = self.removed_device_queue.get()
            logger.info("Remove client: {}".format(sid))
            del self.event_subscribes[sid]
            self.removed_device_queue.task_done()
        # add clients
        while not self.append_device_queue.empty():
            client = self.append_device_queue.get()
            self.event_subscribes[client.sid] = client
            self.append_device_queue.task_done()
        # send stateChangeList to client
        for sid in self.event_subscribes:
            client = self.event_subscribes[sid]
            if client.is_timeout():
                self.remove_subscribe(client.sid)
                continue
            try:
                # Only send state which within the service
                state = {}
                for name in state_change_list:
                    if self.state_list[name].service == client.service:
                        state[name] = state_change_list[name]
                if len(state) == 0:
                    continue
                client.send_event_callback(state)

            except Exception as e:
                logger.error("send event error: " + str(e))
                client.error = client.error + 1
                if client.error > 10:
                    logger.debug("remove " + client.sid)
                    self.remove_subscribe(client.sid)

    def event(self):
        """DLNA Event thread
        If a DLNA client subscribes to the dlna event,
        it will automatically send the event to the client when the renderer state changes.
        Uses a threading.Event instead of a 1s sleep-poll so state changes
        (volume / position / transport) reach the client with sub-second latency.
        """
        while self.running:
            self._state_event.wait(timeout=0.25)
            self._state_event.clear()
            if not self.state_queue.empty():
                state = {}
                while not self.state_queue.empty():
                    k, v = self.state_queue.get()
                    state[k] = v
                    self.state_queue.task_done()
                self.send_states_to_clients(state)

    def call(self, rawbody):
        """Processing requests from DLNA clients
        The request from the client is passed into this method
        through the DLNAHandler(macast.py -> class DLNAHandler).
        If the Render class implements the corresponding action method,
        the method will be called automatically.
        Otherwise, the corresponding state variable will be returned
        according to the **action return value** described in the XML file.
        :param rawbody: soap request from dlna client
        :return:
        """
        try:
            envelope = etree.fromstring(normalize_soap_body(rawbody))
        except etree.XMLSyntaxError as e:
            # Log the head of the offending body -- a client-side XML quirk is
            # otherwise invisible (macast.log is wiped on every app start) --
            # and answer with a SOAP Fault instead of an opaque CherryPy 500
            # HTML page, which no DLNA control point can parse.
            logger.error('SOAP parse failed: {} ({} bytes, head={!r})'.format(
                e, len(rawbody), rawbody[:300]))
            cherrypy.response.status = 500
            return soap_fault(402, 'Invalid Args')
        soap_ns = 'http://schemas.xmlsoap.org/soap/envelope/'
        body = envelope.find('{{{}}}Body'.format(soap_ns))
        if body is None or len(body) == 0:
            raise ValueError('malformed SOAP request: missing Body/action')
        # Skip any comment/processing-instruction nodes before the action.
        action_el = None
        for child in body:
            if isinstance(child.tag, str):
                action_el = child
                break
        if action_el is None:
            raise ValueError('malformed SOAP request: no action element')
        # Use the action element's namespace + local name instead of brittle
        # string splitting on the tag, which breaks on comments/whitespace
        # text nodes or non-standard SOAP namespaces.
        qn = etree.QName(action_el.tag)
        action = qn.localname
        service = qn.namespace.split(':')[-2]
        param = {}
        for node in action_el:
            param[etree.QName(node.tag).localname] = node.text
        method = "{}_{}".format(service, action)
        if method not in [
            'AVTransport_GetPositionInfo',
            'AVTransport_GetTransportInfo',
            'RenderingControl_GetVolume'
        ]:
            logger.info("{} {}".format(method, param))
        res = {}
        service_type = Service.get(service)
        if hasattr(self, method):
            data = {}
            # input = self.action_list[service][action].input
            input = service_type.actions[action].input
            for arg in input:
                data[arg.name] = Argument(
                    arg.name, arg.state,
                    param[arg.name] if arg.name in param else None)
                if arg.name in param:
                    self.set_state(arg.state, param[arg.name])
            res = getattr(self, method)(data)
        else:
            # output = self.action_list[service][action].output
            output = service_type.actions[action].output
            for arg in output:
                res[arg.name] = self.state_list[arg.state].value
        if method not in ['ConnectionManager_GetProtocolInfo', 'AVTransport_GetPositionInfo']:
            logger.info("{}res: {}".format("*" * 20, res))
        else:
            logger.info("{}res: {}".format("*" * 20, method))

        # build response xml
        ns = 'http://schemas.xmlsoap.org/soap/envelope/'
        encoding = 'http://schemas.xmlsoap.org/soap/encoding/'
        root = etree.Element(etree.QName(ns, 'Envelope'), nsmap={'s': ns})
        root.attrib[f'{{{ns}}}encodingStyle'] = encoding
        body = etree.SubElement(root, etree.QName(ns, 'Body'), nsmap={'s': ns})
        # namespace = 'urn:schemas-upnp-org:service:{}:1'.format(service)
        response = etree.SubElement(body,
                                    etree.QName(
                                        service_type.namespace, '{}Response'.format(action)),
                                    nsmap={'u': service_type.namespace})
        for key in res:
            prop = etree.SubElement(response, key)
            prop.text = str(res[key])
        return etree.tostring(root, encoding="UTF-8", xml_declaration=False)

    def set_state(self, name: str, value) -> None:
        """Set DLNA state which defined by xml file
        :param name: state name
        :param value: state value
        :return:
        """
        # update states which will send to DLNA Client
        if name in SERVICE_STATE_OBSERVED['AVTransport'] or \
                name in SERVICE_STATE_OBSERVED['RenderingControl']:
            logger.debug("setState: {} {}".format(name, value))
            # When some states change, the DLNA client needs to be notified immediately
            # We put this kind of state into state_queue, waiting to be sent to client.
            self.state_queue.put((name, value))
            self._state_event.set()
        # update other states
        if self.state_list[name].value != value:
            self.state_list[name].value = value

    def get_state(self, name: str):
        """Get DLNA state, The type of state is described by XML file
        :param name: DLNA state name
        :return: int/string/bool
        """
        return self.state_list[name].value

    def start(self):
        """Start render thread
        """
        if self.running:
            return

        self.running = True
        self.event_thread = threading.Thread(target=self.event, daemon=True)
        self.event_thread.start()
        self.set_state_stop()

    def stop(self):
        """Stop render thread
        """
        self.running = False

    # The following method names are defined by the XML file

    def RenderingControl_SetVolume(self, data):
        volume = data['DesiredVolume']
        self.renderer.set_media_volume(volume.value)
        return {}

    def RenderingControl_SetMute(self, data):
        mute = data['DesiredMute']
        if mute.value == 0 or mute.value == '0':
            mute = False
        else:
            mute = True
        self.renderer.set_media_mute(mute)
        return {}

    def AVTransport_SetAVTransportURI(self, data):
        uri = data['CurrentURI'].value
        logger.info(uri)
        self.set_state_url(uri)
        self.renderer.set_media_url(uri)
        title = Setting.get_friendly_name()
        try:
            # Same tolerance as the SOAP body: a client that indents its
            # envelope indents the embedded DIDL-Lite metadata too, and a
            # leading newline before <?xml...?> would cost us the real title.
            meta = etree.fromstring(
                normalize_soap_body(data['CurrentURIMetaData'].value))
            title_xml = meta.find('.//{{{}}}title'.format(meta.nsmap['dc']))
            if title_xml is not None and title_xml.text is not None:
                title = title_xml.text
            metadata = etree.tostring(meta, encoding="UTF-8", xml_declaration=False)
        except Exception as e:
            logger.error(str(e))
            logger.error(data['CurrentURIMetaData'].value)
            self.set_state('CurrentTrackMetaData', data['CurrentURIMetaData'].value)
        else:
            self.set_state('CurrentTrackMetaData', metadata.decode())
        self.renderer.set_media_title(title)
        self.renderer.set_media_resume()
        self.set_state('CurrentTrackTitle', title)
        self.set_state('CurrentTrackURI', uri)
        # Maintain a simple cast playlist so AVTransport Next/Previous work.
        if uri not in self.playlist:
            self.playlist.append(uri)
        self.current_index = self.playlist.index(uri)
        self.set_state('NumberOfTracks', len(self.playlist))
        self.set_state('CurrentTrack', self.current_index + 1)
        self.set_state('RelativeTimePosition', '00:00:00')
        self.set_state('AbsoluteTimePosition', '00:00:00')
        self.set_state('TransportState', 'PAUSED_PLAYBACK')
        self.set_state('TransportStatus', 'OK')
        self._add_history(uri, title)
        return {}

    def AVTransport_Next(self, data):
        return self._playlist_step(1)

    def AVTransport_Previous(self, data):
        return self._playlist_step(-1)

    def _playlist_step(self, delta):
        if not self.playlist:
            return {}
        idx = self.current_index + delta
        if idx < 0 or idx >= len(self.playlist):
            return {}
        self.current_index = idx
        uri = self.playlist[idx]
        self.set_state_url(uri)
        self.renderer.set_media_url(uri)
        self.set_state('CurrentTrack', idx + 1)
        self.set_state('CurrentTrackURI', uri)
        self.set_state('TransportState', 'PLAYING')
        self.set_state('TransportStatus', 'OK')
        return {}

    def cast_uri(self, uri, title=''):
        """Push a (local or remote) uri to the renderer.

        Used by the local-file-casting flow: the settings page uploads a file,
        Macast serves it from 127.0.0.1, and publishes 'cast_local_file' which
        this method handles."""
        if not uri:
            return
        self.set_state_url(uri)
        self.renderer.set_media_url(uri)
        if title:
            self.renderer.set_media_title(title)
            self.set_state('CurrentTrackTitle', title)
        if uri not in self.playlist:
            self.playlist.append(uri)
        self.current_index = self.playlist.index(uri)
        self.set_state('NumberOfTracks', len(self.playlist))
        self.set_state('CurrentTrack', self.current_index + 1)
        self.set_state('CurrentTrackURI', uri)
        self.set_state('RelativeTimePosition', '00:00:00')
        self.set_state('AbsoluteTimePosition', '00:00:00')
        self.set_state('TransportState', 'PLAYING')
        self.set_state('TransportStatus', 'OK')
        self._add_history(uri, title)

    def _add_history(self, uri, title=''):
        """Persist a played item to the on-disk play history (most recent first).
        Dedupes by uri and caps the list so the settings file stays small."""
        try:
            history = Setting.get(SettingProperty.Play_History, []) or []
            history = [h for h in history
                       if isinstance(h, dict) and h.get('uri') != uri]
            history.insert(0, {
                'uri': uri,
                'title': title or uri,
                'time': int(time.time()),
            })
            history = history[:50]
            Setting.set(SettingProperty.Play_History, history)
        except Exception as e:
            logger.error('add play history error: %s' % e)

    def clear_play_history(self):
        """Remove all persisted play history."""
        try:
            Setting.set(SettingProperty.Play_History, [])
        except Exception as e:
            logger.error('clear play history error: %s' % e)

    def AVTransport_Play(self, data):
        self.renderer.set_media_resume()
        self.set_state('TransportState', 'PLAYING')
        self.set_state('TransportStatus', 'OK')
        return {}

    def AVTransport_Pause(self, data):
        self.renderer.set_media_pause()
        self.set_state('TransportState', 'PAUSED_PLAYBACK')
        return {}

    def AVTransport_Seek(self, data):
        target = data['Target']
        self.renderer.set_media_position(target.value)
        self.set_state('RelativeTimePosition', target.value)
        self.set_state('AbsoluteTimePosition', target.value)
        return {}

    def AVTransport_Stop(self, data):
        self.renderer.set_media_stop()
        self.set_state('TransportState', 'STOPPED')
        return {}

    # The following methods are usually used to update the states of
    # DLNA Renderer according to the status obtained from the player.
    # So, when your player state changes, call the following methods.
    # For example, when you click the pause button of the player,
    # call "self.protocol.set_state_pause()" from renderer
    # Then, the DLNA client (such as your mobile phone) will
    # automatically get this information and update it to the front-end.

    def set_state_position(self, data: str):
        """
        :param data: string, eg: 00:00:00
        :return:
        """
        self.set_state('RelativeTimePosition', data)
        self.set_state('AbsoluteTimePosition', data)

    def set_state_duration(self, data: str):
        """
        :param data: string, eg: 00:00:00
        :return:
        """
        self.set_state('CurrentTrackDuration', data)
        self.set_state('CurrentMediaDuration', data)

    def set_state_pause(self):
        self.set_state_transport('PAUSED_PLAYBACK')

    def set_state_play(self):
        self.set_state_transport('PLAYING')

    def set_state_stop(self):
        self.set_state_transport('STOPPED')

    def set_state_eof(self):
        self.set_state_transport('NO_MEDIA_PRESENT')

    def set_state_transport(self, data: str):
        """
        :param data: string in [PLAYING, PAUSED_PLAYBACK, STOPPED, NO_MEDIA_PRESENT]
        :return:
        """
        self.set_state('TransportState', data)
        self.set_state('TransportStatus', 'OK')

    def set_state_transport_error(self):
        """
        :return:
        """
        self.set_state('TransportState', 'STOPPED')
        self.set_state('TransportStatus', 'ERROR_OCCURRED')

    def set_state_mute(self, data: bool):
        """
        :param data: bool
        :return:
        """
        self.set_state('Mute', data)

    def set_state_volume(self, data: int):
        """
        :param data: int, range from 0 to 100
        :return:
        """
        self.set_state('Volume', data)

    def set_state_speed(self, data: str):
        self.set_state('TransportPlaySpeed', data)

    def set_state_display_subtitle(self, data: bool):
        self.set_state('DisplayCurrentSubtitle', data)

    def set_state_url(self, data: str):
        self.set_state('CurrentTrackURI', data)

    # When you are implementing another protocol similar to DLNA,
    # you can get the status of DLNA renderer by calling the following methods.
    # Using DLNA protocol usually does not need to pay attention to these methods,
    # because the state of renderer will be read automatically when using DLNA protocol.

    def get_state_title(self) -> str:
        """
        :return: string, eg: demo
        """
        return self.get_state('CurrentTrackTitle')

    def get_state_url(self) -> str:
        """
        :return: string, eg: http://10.10.10.10/demo.mp4
        """
        return self.get_state('CurrentTrackURI')

    def get_state_position(self) -> str:
        """
        :return: string, eg: 00:00:00
        """
        return self.get_state('RelativeTimePosition')

    def get_state_duration(self) -> str:
        """
        :return: string, eg: 00:00:00
        """
        return self.get_state('CurrentMediaDuration')

    def get_state_volume(self) -> int:
        """
        :return: int, range from 0 to 100
        """
        return self.get_state('Volume')

    def get_state_mute(self) -> bool:
        """
        :return: bool
        """
        return self.get_state('Mute')

    def get_state_transport_state(self) -> str:
        """
        :return: string in [PLAYING, PAUSED_PLAYBACK, STOPPED, NO_MEDIA_PRESENT]
        """
        return self.get_state('TransportState')

    def get_state_transport_status(self) -> str:
        """
        :return: string in [OK, ERROR_OCCURRED]
        """
        return self.get_state('TransportStatus')

    def get_state_speed(self) -> str:
        return self.get_state('TransportPlaySpeed')

    def get_state_display_subtitle(self) -> bool:
        return bool(self.get_state('DisplayCurrentSubtitle'))


@cherrypy.expose
class Handler:

    def __init__(self):
        self.setting_page = load_xml(XMLPath.SETTING_PAGE.value).encode()
        # Random token used to protect management endpoints (install-plugin,
        # save-launch-param, status/log/launch-param queries) when the request
        # comes from outside the loopback interface. The local settings page is
        # always served from 127.0.0.1, so it never needs to send the token.
        self._api_token = secrets.token_hex(16)
        self.local_dir = os.path.join(SETTING_DIR, 'local_files')
        os.makedirs(self.local_dir, exist_ok=True)

    @property
    def protocol(self) -> Protocol:
        protocols = cherrypy.engine.publish('get_protocol')
        if len(protocols) == 0:
            logger.error("Unable to find an available protocol.")
            return Protocol()
        return protocols.pop()

    def _management_allowed(self):
        """Management endpoints must only be reachable from an authenticated
        channel. A channel is trusted when it is:
          - the loopback interface (the desktop app talking to itself), or
          - received over HTTPS (the admin explicitly opened the https
            endpoint), or
          - carrying a valid X-Macast-Token.
        DLNA control/SUBSCRIBE traffic from the LAN stays open, since that is
        the whole point of a renderer."""
        remote = getattr(cherrypy.request, 'remote', None)
        ip = getattr(remote, 'ip', '127.0.0.1')
        if ip in ('127.0.0.1', '::1', '::ffff:127.0.0.1', 'localhost'):
            return True
        # A request that arrived over the HTTPS admin channel is treated as
        # authenticated: the user explicitly opened https://host:port.
        if getattr(cherrypy.request, 'scheme', 'http') == 'https':
            return True
        token = cherrypy.request.headers.get('X-Macast-Token')
        if not token:
            token = cherrypy.request.params.get('token')
        return bool(token and token == self._api_token)

    def reload(self):
        cherrypy.server.httpserver = _cpnative_server.CPHTTPServer(cherrypy.server)

    # -- plugin hot-plug ---------------------------------------------------

    def _plugin_manager(self):
        """The live plugin manager, or None if the app has not published one."""
        manager = cherrypy_publish('get_plugin_manager', None)
        if manager is None:
            logger.error("Plugin manager unavailable")
            raise ValueError('插件管理不可用')
        return manager

    def _plugin_change(self, action, **kwargs):
        """Run one hot-plug action and return a JSON-ready result dict.

        All four verbs (enable / disable / uninstall / install) share this so
        the settings file, the plugin directory and the running services are
        updated in one place, and the app is told to re-apply afterwards. No
        restart is involved -- that is the point of hot-plug.
        """
        manager = self._plugin_manager()
        try:
            if action == 'enable' or action == 'disable':
                key = kwargs.get('key') or ''
                plugin = manager.plugin_by_key(key)
                if plugin is None:
                    raise ValueError('未找到该插件')
                if plugin.kind() == 'protocol':
                    manager.set_protocol_enabled(plugin.title, action == 'enable')
                else:
                    manager.set_renderer_enabled(key, action == 'enable')
                message = '{}已{}'.format(plugin.title,
                                          '启用' if action == 'enable' else '停用')
            elif action == 'uninstall':
                plugin = manager.uninstall(kwargs.get('key') or '')
                message = '已卸载 {}'.format(plugin.title)
            elif action == 'install':
                if kwargs.get('path'):
                    plugin = manager.install_file(kwargs['path'],
                                                  kwargs.get('type', 'renderer'),
                                                  kwargs.get('filename'))
                else:
                    plugin = manager.install_url(kwargs.get('url', ''),
                                                 kwargs.get('type', 'renderer'))
                message = '已安装 {}'.format(plugin.title)
            else:
                raise ValueError('未知操作：{}'.format(action))
        except ValueError as e:
            return {'code': 1, 'message': str(e)}
        except Exception as e:
            logger.error("Plugin %s failed: %s", action, e)
            return {'code': 1, 'message': '{}失败：{}'.format(action, e)}
        cherrypy.engine.publish('plugins_changed')
        return {'code': 0, 'message': message}

    def __install_uploaded_plugin(self, upload, kwargs):
        """Install a plugin the user uploaded from their own machine.

        The upload is staged in a temp path first and removed afterwards, so a
        file that fails to load leaves nothing behind; `install_file` is the
        one place that copies into the plugin dir and enables it.
        """
        filename = os.path.basename(getattr(upload, 'filename', '') or '')
        if not filename:
            return json.dumps({'code': 1, 'message': '未收到插件文件'},
                              indent=4).encode()
        plugin_type = kwargs.get('plugin-type') or 'renderer'
        staging_dir = os.path.join(SETTING_DIR, 'local_files')
        staged = os.path.join(staging_dir, filename)
        try:
            os.makedirs(staging_dir, exist_ok=True)
            with open(staged, 'wb') as f:
                f.write(upload.file.read())
        except Exception as e:
            logger.error('plugin upload staging failed: %s', e)
            return json.dumps({'code': 1, 'message': '插件上传失败'},
                              indent=4).encode()
        result = self._plugin_change('install', path=staged, type=plugin_type,
                                     filename=filename)
        try:
            os.remove(staged)
        except OSError:
            pass
        return json.dumps(result, indent=4).encode()

    def _set_network_interface(self, name):
        """Pin (or clear) the interface used for discovery and re-advertise.

        An unknown interface name is refused rather than stored: a typo would
        otherwise silently drop Macast to the loopback fallback and make it
        invisible while looking like it was configured.
        """
        name = (name or '').strip()
        if name:
            known = {row['name'] for row in Setting.network_interfaces()}
            if name not in known:
                return {'code': 1,
                        'message': '未找到网卡 {}，当前可用：{}'.format(
                            name, '、'.join(sorted(known)) or '无')}
        Setting.set_network_interface(name)
        cherrypy.engine.publish('network_interface_changed')
        return {'code': 0,
                'message': '已切换到 {}'.format(name or '自动选择'),
                'interface': name}


    def get_status(self):
        """Snapshot of the running service, the media being cast, and the
        connected DLNA clients, for the settings page 'Status' tab."""
        protocol = self.protocol
        server = {
            'running': Setting.is_service_running(),
            'port': Setting.get_port(),
            'version': Setting.get_version(),
            'friendly_name': Setting.get_friendly_name(),
            'renderer': Setting.get(SettingProperty.Macast_Renderer, ''),
            # Protocols may now run concurrently; report whichever are enabled
            # (falling back to the legacy single value for older installs).
            'protocol': Setting.get(SettingProperty.Macast_Protocols, [])
            or ([Setting.get(SettingProperty.Macast_Protocol, '')]
                if Setting.has(SettingProperty.Macast_Protocol) else []),
            'platform': sys.platform,
            'system': Setting.get_system(),
            'system_version': Setting.get_system_version(),
            # Only the addresses a phone can actually reach; Setting.get_ip()
            # also lists VM bridges and Tailscale tunnels, which is noise here
            # and actively misleading when diagnosing "I can see it but casting
            # fails" (the sender may have picked one of the dead addresses).
            'ip': '/'.join(advertisable_addresses()),
            'https_enabled': Setting.is_https_enabled(),
            'https_port': Setting.get_https_port() if Setting.is_https_enabled() else None,
        }
        network = {
            # '' means automatic (default-route interface).
            'interface': Setting.get_network_interface(),
            'advertised': advertisable_addresses(),
            'interfaces': Setting.network_interfaces(),
        }
        media = {}
        for name in ('CurrentURI', 'CurrentTrackURI', 'CurrentTrackTitle',
                     'CurrentTrackDuration', 'CurrentMediaDuration',
                     'AbsoluteTimePosition', 'RelativeTimePosition',
                     'TransportState', 'TransportStatus', 'TransportPlaySpeed',
                     'CurrentPlayMode', 'Volume', 'Mute', 'NumberOfTracks',
                     'CurrentTrack', 'DisplayCurrentSubtitle'):
            try:
                media[name] = protocol.get_state(name)
            except Exception:
                media[name] = None
        clients = []
        try:
            # getattr, not direct access: with only Chromecast/AirPlay enabled
            # the ProtocolGroup delegates this to a protocol that has no
            # `event_subscribes` at all, and the AttributeError made the whole
            # status endpoint noisy for no reason.
            for sid, c in getattr(protocol, 'event_subscribes', {}).items():
                clients.append({
                    'host': getattr(c, 'host', ''),
                    'url': getattr(c, 'url', ''),
                    'service': getattr(c, 'service', ''),
                    'sid': getattr(c, 'sid', ''),
                    'path': getattr(c, 'path', ''),
                    'timeout': getattr(c, 'timeout', 0),
                })
        except Exception as e:
            logger.error(e)
        history = []
        try:
            history = Setting.get(SettingProperty.Play_History, []) or []
        except Exception:
            pass
        return {'server': server, 'media': media, 'clients': clients,
                'history': history, 'network': network}

    def GET(self, param=None, *args, **kwargs):
        if not Setting.is_service_running():
            raise cherrypy.HTTPError(503, 'Server restarting')
        if param == 'api':
            cherrypy.response.headers['Content-Type'] = 'application/json;charset:utf-8'
            query = kwargs.get('query', '')
            # Sensitive management queries: block unless local or token-bearing.
            if query in ('status', 'log', 'launch-param', 'interfaces') \
                    and not self._management_allowed():
                return json.dumps({'code': 403,
                                   'message': 'Forbidden: management API requires local access or token'},
                                  indent=4).encode()
            res = {
                'api?query=log': 'get logs of macast',
                'api?query=settings': 'get settings of macast',
            }
            if query == 'log':
                log_path = os.path.join(SETTING_DIR, 'macast.log')
                data = ''
                try:
                    with open(log_path, 'r', encoding='utf-8') as f:
                        data = f.read()
                except:
                    pass
                res = {"logs": data}
            elif query == 'launch-param':
                res = Setting.setting
            elif query == 'plugin-info':
                info = cherrypy_publish('get_plugin_info', [])
                res = {
                    'platform': sys.platform,
                    'version': Setting.version,
                    'plugins': info
                }
            elif query == 'status':
                res = self.get_status()
            elif query == 'interfaces':
                # Backs the settings page network picker. `usable`/`reason`
                # come from the same filter discovery uses, so the page never
                # offers an interface the advertiser would refuse to publish.
                res = {
                    'current': Setting.get_network_interface(),
                    'advertised': advertisable_addresses(),
                    'interfaces': Setting.network_interfaces(),
                }
            return json.dumps(res, indent=4).encode()
        if param == 'local':
            # Serve a previously uploaded local file (path-traversal safe).
            fname = kwargs.get('file', '')
            if not fname or '/' in fname or '\\' in fname or '..' in fname:
                raise cherrypy.HTTPError(400, 'Bad file name')
            fpath = os.path.join(self.local_dir, os.path.basename(fname))
            if not os.path.exists(fpath):
                raise cherrypy.HTTPError(404, 'File not found')
            cherrypy.response.headers['Content-Type'] = 'application/octet-stream'
            with open(fpath, 'rb') as f:
                return f.read()
        if param == 'sw.js':
            # Served from root so the service worker controls scope "/".
            cherrypy.response.headers['Content-Type'] = 'application/javascript; charset=utf-8'
            return load_xml(XMLPath.SW_JS.value).encode('utf-8')
        if param == 'manifest.webmanifest':
            cherrypy.response.headers['Content-Type'] = 'application/manifest+json; charset=utf-8'
            return load_xml(XMLPath.MANIFEST.value).encode('utf-8')
        if param is not None:
            raise cherrypy.HTTPRedirect('/')
        cherrypy.response.headers['Content-Type'] = 'text/html'
        # Cached at init time; avoids re-reading the file from disk on every GET.
        return self.setting_page

    #: Parameters that mutate state. They are only honoured from a trusted
    #: channel (loopback / HTTPS / valid token) -- see `_management_allowed`.
    _MANAGEMENT_PARAMS = ('save-launch-param', 'install-plugin', 'plugin-enable',
                          'plugin-disable', 'plugin-uninstall', 'set-interface')

    def POST(self, *args, **kwargs):
        cherrypy.response.headers['Content-Type'] = 'application/json;charset:utf-8'
        res = {'code': 0, 'message': 'success'}
        # Uploads are dispatched by *field name*: the cast flow, the subtitle
        # flow and plugin installation all post a file to the same endpoint.
        file_part = None
        file_field = ''
        for key, v in kwargs.items():
            # An uploaded file part has .file (file-like) and .filename; a
            # plain form field is just a string, so detect by attribute.
            if hasattr(v, 'file') and hasattr(v, 'filename'):
                file_part, file_field = v, key
                break
        if file_part is not None:
            if not self._management_allowed():
                res['code'] = 403
                res['message'] = 'Forbidden: management API requires local access or token'
                return json.dumps(res, indent=4).encode()
            if file_field == 'plugin-file':
                return self.__install_uploaded_plugin(file_part, kwargs)
            upload = file_part
            filename = os.path.basename(getattr(upload, 'filename', 'cast.bin'))
            ext = os.path.splitext(filename)[1].lower()
            sub_exts = ('.srt', '.ass', '.ssa', '.vtt', '.sub', '.sup', '.idx')
            try:
                path = os.path.join(self.local_dir, filename)
                with open(path, 'wb') as f:
                    f.write(upload.file.read())
                url = 'http://127.0.0.1:{}/?local&file={}'.format(Setting.get_port(), filename)
                if ext in sub_exts:
                    # Subtitle file: attach to the currently playing media.
                    res['url'] = url
                    cherrypy.engine.publish('set_media_sub_file',
                                            {'url': url, 'title': filename})
                else:
                    res['url'] = url
                    cherrypy.engine.publish('cast_local_file', url, filename)
            except Exception as e:
                logger.error('local file upload error: %s' % e)
                res['code'] = 1
                res['message'] = 'upload failed'
            return json.dumps(res, indent=4).encode()
        # Management endpoints: only allow from loopback or with a valid token.
        if any(kwargs.get(p, None) is not None for p in self._MANAGEMENT_PARAMS):
            if not self._management_allowed():
                res['code'] = 403
                res['message'] = 'Forbidden: management API requires local access or token'
                return json.dumps(res, indent=4).encode()
        if kwargs.get('set-interface', None) is not None:
            res = self._set_network_interface(kwargs.get('set-interface'))
        elif kwargs.get('plugin-enable', None) is not None:
            res = self._plugin_change('enable', key=kwargs.get('plugin-key', ''))
        elif kwargs.get('plugin-disable', None) is not None:
            res = self._plugin_change('disable', key=kwargs.get('plugin-key', ''))
        elif kwargs.get('plugin-uninstall', None) is not None:
            res = self._plugin_change('uninstall', key=kwargs.get('plugin-key', ''))
        elif kwargs.get('save-launch-param', None) is not None:
            setting = kwargs.get('save-launch-param', None)
            try:
                setting = json.loads(setting)
            except Exception as e:
                res['code'] = 1
                res['message'] = 'json format error'
            else:
                Setting.setting = setting
                Setting.save()
                Setting.restart()
                # cherrypy.engine.restart()
        elif kwargs.get('install-plugin', None) is not None:
            # Accept both the flat form fields the settings page sends and the
            # older JSON blob, so an already-open page keeps working.
            raw = kwargs.get('install-plugin')
            payload = {}
            if isinstance(raw, str) and raw.strip().startswith('{'):
                try:
                    payload = json.loads(raw)
                except ValueError:
                    return json.dumps({'code': 1, 'message': 'json format error'},
                                      indent=4).encode()
            res = self._plugin_change(
                'install',
                url=kwargs.get('plugin-url') or payload.get('url', ''),
                type=kwargs.get('plugin-type') or payload.get('type', 'renderer'))
        elif kwargs.get('set-subtitle-show', None) is not None:
            if not self._management_allowed():
                res['code'] = 403
                res['message'] = 'Forbidden: management API requires local access or token'
                return json.dumps(res, indent=4).encode()
            show = str(kwargs.get('set-subtitle-show')).lower() in ('1', 'true', 'yes', 'on')
            cherrypy.engine.publish('set_media_sub_show', show)
        elif kwargs.get('cast-uri', None) is not None:
            if not self._management_allowed():
                res['code'] = 403
                res['message'] = 'Forbidden: management API requires local access or token'
                return json.dumps(res, indent=4).encode()
            try:
                self.protocol.cast_uri(kwargs.get('cast-uri'))
            except Exception as e:
                logger.error('cast uri error: %s' % e)
                res['code'] = 1
                res['message'] = 'cast failed'
        elif kwargs.get('clear-play-history', None) is not None:
            if not self._management_allowed():
                res['code'] = 403
                res['message'] = 'Forbidden: management API requires local access or token'
                return json.dumps(res, indent=4).encode()
            try:
                self.protocol.clear_play_history()
            except Exception as e:
                logger.error('clear play history error: %s' % e)
                res['code'] = 1
                res['message'] = 'clear failed'
        else:
            logger.info(kwargs)

        return json.dumps(res, indent=4).encode()


@cherrypy.expose
class DLNAHandler(Handler):
    """Receiving requests from DLNA client
    and communicating with the RenderPlugin thread
    see also: plugin.py -> class RenderPlugin
    """

    def __init__(self):
        super(DLNAHandler, self).__init__()
        self.description = None
        self.reload()

    def reload(self):
        super(DLNAHandler, self).reload()
        self.build_description()

    @property
    def protocol(self) -> DLNAProtocol:
        protocols = cherrypy.engine.publish('get_protocol')
        if len(protocols) == 0:
            logger.error("Unable to find an available protocol.")
            return DLNAProtocol()
        return protocols.pop()

    def build_description(self):
        self.description = load_xml(XMLPath.DESCRIPTION.value).format(
            friendly_name=Setting.get_friendly_name(),
            manufacturer="xfangfang",
            manufacturer_url="https://github.com/xfangfang",
            model_description="AVTransport Media Renderer",
            model_name="Macast",
            model_url="https://xfangfang.github.io/Macast",
            model_number=Setting.get_version(),
            uuid=Setting.get_usn(),
            serial_num=1024,
            header_extra="",
            service_extra=""
        ).encode()

    def GET(self, param=None, *args, **kwargs):
        if param == 'description.xml':
            return self.description
        return super(DLNAHandler, self).GET(param, *args, **kwargs)

    def POST(self, service=None, param=None, *args, **kwargs):
        length = cherrypy.request.headers['Content-Length']
        rawbody = cherrypy.request.body.read(int(length))
        logger.debug('RAW: {}'.format(rawbody))
        if param == 'action':
            res = self.protocol.call(rawbody)
            cherrypy.response.headers['EXT'] = ''
            logger.debug('RES: {}'.format(res))
            return res
        return super(DLNAHandler, self).POST(service, param, *args, **kwargs)

    def SUBSCRIBE(self, service="", param=""):
        """DLNA/UPNP event subscribe
        """
        if param == 'event':
            SID = cherrypy.request.headers.get('SID')
            CALLBACK = cherrypy.request.headers.get('CALLBACK')
            TIMEOUT = cherrypy.request.headers.get('TIMEOUT')
            TIMEOUT = TIMEOUT if TIMEOUT is not None else 'Second-1800'
            TIMEOUT = int(TIMEOUT.split('-')[-1])
            if SID:
                logger.error("RENEW SUBSCRIBE:!!!!!!!" + service)
                res = self.protocol.renew_subscribe(SID, TIMEOUT)
                if res != 200:
                    logger.error("RENEW SUBSCRIBE: cannot find such sid.")
                    raise cherrypy.HTTPError(status=res)
                cherrypy.response.headers['SID'] = SID
                cherrypy.response.headers['TIMEOUT'] = TIMEOUT
            elif CALLBACK:
                logger.error("ADD SUBSCRIBE:!!!!!!!" + service)
                suburl = re.findall("<(.*?)>", CALLBACK)[0]
                res = self.protocol.add_subscribe(service, suburl, TIMEOUT)
                cherrypy.response.headers['SID'] = res['SID']
                cherrypy.response.headers['TIMEOUT'] = res['TIMEOUT']
            else:
                logger.error("SUBSCRIBE: cannot find sid and callback.")
                raise cherrypy.HTTPError(status=412)
        return b''

    def UNSUBSCRIBE(self, service, param):
        """DLNA/UPNP event unsubscribe
        """
        if param == 'event':
            SID = cherrypy.request.headers.get('SID')
            if SID:
                logger.error("REMOVE SUBSCRIBE:!!!!!!!" + service)
                res = self.protocol.remove_subscribe(SID)
                if res != 200:
                    raise cherrypy.HTTPError(status=res)
                return b''
        logger.error("UNSUBSCRIBE: error 412.")
        raise cherrypy.HTTPError(status=412)
