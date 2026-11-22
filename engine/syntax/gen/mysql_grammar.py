"""MySQL syntax TiDB's grammar lacks, written from the MySQL 8.4 Reference Manual's syntax of each
statement, and applied to TiDB's grammar once its actions are removed.

Where MySQL's syntax would need two tokens of lookahead in an LALR grammar, the rule here reads a
little more than MySQL takes (an event's clauses in any order, say); what it reads is never less.
"""
import re

# Keywords MySQL has that TiDB's grammar doesn't, each with whether MySQL reserves it.
KEYWORDS = [
    ("AGGREGATE", False), ("AT", False), ("COMPLETION", False), ("CONDITION", True), ("CONTAINS", False),
    ("DETERMINISTIC", True), ("DIAGNOSTICS", False), ("EACH", True), ("ENDS", False), ("EVERY", False),
    ("FOLLOWS", False), ("GET", True), ("LOOP", True), ("MODIFIES", True), ("PRECEDES", False),
    ("READS", True), ("RESIGNAL", True), ("RETURNS", False), ("SIGNAL", True), ("SONAME", False),
    ("STACKED", False), ("STARTS", False), ("STRING", False), ("UNDO", True),
    ("BEFORE", True), ("CHANGED", False), ("DUMPFILE", False), ("GEOMCOLLECTION", False), ("GEOMETRY", False),
    ("GEOMETRYCOLLECTION", False), ("LINESTRING", False), ("MANUAL", False), ("MULTILINESTRING", False),
    ("MULTIPOINT", False), ("MULTIPOLYGON", False), ("POLYGON", False), ("SRID", False), ("UPGRADE", False),
    ("USE_FRM", False), ("WORK", False),
    ("ACTIVE", False), ("CODE", False), ("COMPONENT", False), ("DATAFILE", False), ("EMPTY", False), ("EXPORT", False),
    ("GTIDS", False), ("INACTIVE", False), ("INSTALL", False), ("JSON_TABLE", True), ("MIGRATE", False),
    ("NESTED", False), ("ONE", False), ("OPTIMIZER_COSTS", False), ("ORDINALITY", False), ("PATH", False),
    ("PERSIST", False), ("PERSIST_ONLY", False), ("PHASE", False), ("PLUGIN", False), ("PREV", False),
    ("SUSPEND", False), ("UNINSTALL", False), ("XA", False), ("XID", False),
    ("MUTEX", False), ("QUALIFY", True), ("RANDOM", False), ("USER_RESOURCES", False),
    ("ZONE", False), ("GROUPING", True),
]




# A label is a name or a keyword MySQL doesn't reserve, but for those a stored program's grammar
# reads in the place a label could start: where a block opens, ends or declares, and the words a
# type, a cast or a trigger's order may end with.
NOT_LABELS = {"BEGIN", "END", "DECLARE", "ASCII", "BYTE", "UNICODE", "SIGNED", "UNSIGNED", "ZEROFILL", "CHARSET", "FOLLOWS", "PRECEDES"}


def names_before_paren():
    """The keywords added here that MySQL doesn't reserve: before `(` each is a function's name, as
    a word TiDB's scanner knows no keyword for is, since no rule here reads one before `(`."""
    return [w for w, reserved in KEYWORDS if not reserved]


def token(word):
    """The grammar's name for a keyword added here."""
    return "kw" + "".join(part.capitalize() for part in word.split("_"))


# Statements MySQL takes in a stored program's body: every statement but these.
NOT_IN_BODIES = {"EmptyStmt", "BeginTransactionStmt", "CreateProcedureStmt", "CreateFunctionStmt", "CreateTriggerStmt", "CreateEventStmt"}

# Rules given alternatives they lack.
EXTEND = {
    "Statement": ["CacheIndexStmt", "LoadIndexStmt", "CreateFunctionStmt", "CreateTriggerStmt", "CreateEventStmt", "AlterEventStmt", "AlterRoutineStmt", "AlterViewStmt", "DropFunctionStmt", "DropTriggerStmt", "DropEventStmt", "SignalStmt", "ResignalStmt", "GetDiagnosticsStmt", "CheckTableStmt", "TableRepairStmt", "ChecksumTableStmt", "LockInstanceStmt", "UnlockInstanceStmt", "HandlerStmt", "XaStmt", "ResetPersistStmt", "ResetLogsStmt", "ComponentStmt", "CreateTablespaceStmt", "AlterTablespaceStmt", "DropTablespaceStmt"],
    "ProcedureDecl": ['"DECLARE" Identifier "CONDITION" "FOR" ProcedurceCond'],
    "ProcedureHandlerType": ['"UNDO"'],
    "ProcedureHcond": ["Identifier"],
    "ProcedureUnlabelLoopStmt": ['"LOOP" ProcedureProcStmt1s "END" "LOOP"'],
    "ProcedureProcStmt": ['"RETURN" Expression'],
    "Type": ["SpatialType"],
    "ConstraintElem": ['"SPATIAL" KeyOrIndexOpt IndexName \'(\' IndexPartSpecificationList \')\' IndexOptionList'],
    "RenameTableStmt": ['"RENAME" "TABLES" TableToTableList'],
    "AlterUserStmt": ['"ALTER" "USER" IfExists Username "DEFAULT" "ROLE" SetDefaultRoleOpt'],
    "TableLock": ['TableName "AS" Identifier LockType'],
    "SetStmt": ['"SET" "PASSWORD" "TO" "RANDOM"', '"SET" "PASSWORD" "FOR" Username "TO" "RANDOM"', '"SET" "PASSWORD" EqOrAssignmentEq PasswordOpt "REPLACE" stringLit', '"SET" "PASSWORD" EqOrAssignmentEq PasswordOpt "REPLACE" stringLit "RETAIN" "CURRENT" "PASSWORD"', '"SET" "PASSWORD" "FOR" Username EqOrAssignmentEq PasswordOpt "REPLACE" stringLit', '"SET" "PASSWORD" "FOR" Username EqOrAssignmentEq PasswordOpt "REPLACE" stringLit "RETAIN" "CURRENT" "PASSWORD"'],
    "VariableAssignment": ['"PERSIST" VariableName EqOrAssignmentEq SetExpr', '"PERSIST_ONLY" VariableName EqOrAssignmentEq SetExpr'],
    # A derived table may name its columns, `(SELECT …) AS t(a, b)`.
    "TableFactor": ["JsonTable", "SubSelect TableAsName '(' IdentList ')'"],
    "JoinTable": ['TableRef "NATURAL" "INNER" "JOIN" TableRef'],
    "CastType": ["NChar OptFieldLen", '"DOUBLE" "PRECISION"'],
    "FunctionCallKeyword": ['"GROUPING" \'(\' ExpressionList \')\''],
    # WEIGHT_STRING's internal form, which mysqldump and the server's own tests write.
    "FunctionCallNonKeyword": ['"WEIGHT_STRING" \'(\' Expression \',\' Expression \',\' Expression \',\' Expression \')\''],
    # JSON_VALUE takes a type and what to do on an empty or wrong value, written as an alternative
    # over the argument list a call reads, so the comma after the first argument stays the list's.
    "FunctionCallGeneric": ['identifier \'(\' ExpressionList "RETURNING" CastType JsonValueOn \')\''],
    "GroupByClause": ['"GROUP" "BY" "ROLLUP" \'(\' ByList \')\''],
    # A parenthesized query takes INTO after its ORDER BY and LIMIT.
    "SetOprStmtWithLimitOrderBy": ['SubSelect "INTO" SelectIntoTarget', 'SubSelect OrderBy "INTO" SelectIntoTarget', 'SubSelect SelectStmtLimit "INTO" SelectIntoTarget', 'SubSelect OrderBy SelectStmtLimit "INTO" SelectIntoTarget'],
    # A session's id as a variable, or in a stored program as one of its own.
    "KillStmt": ["KillOrKillTiDB singleAtIdentifier", 'KillOrKillTiDB "CONNECTION" singleAtIdentifier', 'KillOrKillTiDB "QUERY" singleAtIdentifier', "KillOrKillTiDB identifier"],
    "FlushOption": ['"OPTIMIZER_COSTS"', '"USER_RESOURCES"', 'TableOrTables TableNameList "FOR" "EXPORT"'],
    "ColumnOption": ['"SRID" NUM'],
    "BeginTransactionStmt": ['"BEGIN" "WORK"', '"START" "TRANSACTION" "READ" "ONLY" \',\' "WITH" "CONSISTENT" "SNAPSHOT"', '"START" "TRANSACTION" "READ" "WRITE" \',\' "WITH" "CONSISTENT" "SNAPSHOT"', '"START" "TRANSACTION" "WITH" "CONSISTENT" "SNAPSHOT" \',\' "READ" "ONLY"', '"START" "TRANSACTION" "WITH" "CONSISTENT" "SNAPSHOT" \',\' "READ" "WRITE"'],
    "CommitStmt": ['"COMMIT" "WORK"', '"COMMIT" "WORK" CompletionTypeWithinTransaction'],
    "RollbackStmt": ['"ROLLBACK" "WORK"', '"ROLLBACK" "WORK" CompletionTypeWithinTransaction'],
    "ShowStmt": ['"SHOW" "CREATE" "FUNCTION" TableName', '"SHOW" "CREATE" "TRIGGER" TableName', '"SHOW" "CREATE" "EVENT" TableName', '"SHOW" "PROCEDURE" "CODE" TableName', '"SHOW" "FUNCTION" "CODE" TableName', '"SHOW" "WARNINGS" "LIMIT" LimitOption', '"SHOW" "WARNINGS" "LIMIT" LimitOption \',\' LimitOption', '"SHOW" "ERRORS" "LIMIT" LimitOption', '"SHOW" "ERRORS" "LIMIT" LimitOption \',\' LimitOption', '"SHOW" "ENGINE" Identifier "STATUS"', '"SHOW" "ENGINE" Identifier "MUTEX"'],
    "SimpleExpr": ['builtinCast \'(\' Expression "AT" "TIME" "ZONE" stringLit "AS" CastType \')\'', 'builtinCast \'(\' Expression "AT" "TIME" "ZONE" "INTERVAL" stringLit "AS" CastType \')\'', '"MATCH" \'(\' ColumnNameList \')\' "AGAINST" \'(\' BitExpr FulltextSearchModifierOpt \')\'', '"MATCH" ColumnNameList "AGAINST" \'(\' BitExpr FulltextSearchModifierOpt \')\''],
    "AuthOption": ['"IDENTIFIED" "BY" "RANDOM" "PASSWORD"', '"IDENTIFIED" "BY" AuthString "REPLACE" AuthString', '"IDENTIFIED" "WITH" AuthPlugin "BY" "RANDOM" "PASSWORD"'],
}

# Rules replaced whole: each name and its alternatives, "" for the empty one.
REPLACE = {
    # MySQL takes INTO right after the select list as well as at the end, and HAVING without FROM.
    "SelectStmtBasic": ['"SELECT" SelectStmtOpts SelectStmtFieldList SelectIntoEarly'],
    # INTO goes before or after the locking clauses, of which there may be several.
    "SelectStmtIntoOption": ["", '"INTO" SelectIntoTarget', '"INTO" SelectIntoTarget SelectLockList'],
    "SelectLockOpt": ["", "SelectLockList"],
    # MySQL 8.4 has no SQL_CACHE, where the word is a name; SQL_NO_CACHE it keeps, doing nothing.
    "SelectStmtSQLCache": ['"SQL_NO_CACHE"'],
    # ESCAPE takes any simple expression.
    "LikeOrIlikeEscapeOpt": ["%prec empty", '"ESCAPE" SimpleExpr'],
    "ExplainFormatType": ["Identifier"],
    "ProcedurceLabelOpt": ["", "LabelName"],
    "ProcedureFetchList": ["Identifier", "ProcedureFetchList ',' Identifier"],
    "CreateProcedureStmt": ['"CREATE" OrReplace ViewAlgorithm ViewDefiner ViewSQLSecurity "PROCEDURE" IfNotExists TableName \'(\' OptSpPdparams \')\' RoutineCharacteristics ProcedureProcStmt'],
}

# Text replaced in the rules, each where it appears.
SUBSTITUTE = [
    ("SelectStmtBasic WhereClauseOptional SelectStmtGroup OrderByOptional", "SelectStmtBasic WhereClauseOptional SelectStmtGroup HavingClause OrderByOptional"),
    ("SelectStmtFromDualTable SelectStmtGroup OrderByOptional", "SelectStmtFromDualTable SelectStmtGroup HavingClause OrderByOptional"),
    ('"HISTOGRAM" "ON" IdentList AnalyzeOptionListOpt', '"HISTOGRAM" "ON" IdentList AnalyzeOptionListOpt HistogramUpdate'),
    ("SpOptInout Identifier Type", "SpOptInout Identifier RoutineType"),
    # ILIKE is TiDB's operator, which MySQL doesn't have: there the word is a name.
    ("|\tBitExpr IlikeOrNotOp SimpleExpr LikeOrIlikeEscapeOpt\n\t{}\n", ""),
    # MATCH … AGAINST is a simple expression in MySQL, so it takes a comparison after it.
    ('|\t"MATCH" \'(\' ColumnNameList \')\' "AGAINST" \'(\' BitExpr FulltextSearchModifierOpt \')\'\n\t{}\n', ''),
    # A label is any name a statement can't start with, keywords MySQL doesn't reserve included.
    ("identifier ':' ProcedureBlockContent ProcedurceLabelOpt", "LabelName ':' ProcedureBlockContent ProcedurceLabelOpt"),
    ("identifier ':' ProcedureUnlabelLoopStmt ProcedurceLabelOpt", "LabelName ':' ProcedureUnlabelLoopStmt ProcedurceLabelOpt"),
    ('"ITERATE" identifier', '"ITERATE" LabelName'),
    ('"LEAVE" identifier', '"LEAVE" LabelName'),
    ('"REVOKE" RoleOrPrivElemList "FROM" UsernameList', '"REVOKE" IfExists RoleOrPrivElemList "FROM" UsernameList'),
    ("SelectStmtGroup HavingClause WindowClauseOptional", "SelectStmtGroup HavingClause WindowClauseOptional QualifyClause"),
    ('"REVOKE" RoleOrPrivElemList "ON" ObjectType PrivLevel "FROM" UserSpecList', '"REVOKE" IfExists RoleOrPrivElemList "ON" ObjectType PrivLevel "FROM" UserSpecList IgnoreUnknownUser'),
    ('"CREATE" "USER" IfNotExists UserSpecList RequireClauseOpt', '"CREATE" "USER" IfNotExists UserSpecList DefaultRoleOpt RequireClauseOpt'),
    ('"DECLARE" ProcedureDeclIdents Type ProcedureOptDefault', '"DECLARE" ProcedureDeclIdents RoutineType ProcedureOptDefault'),
    # A CHAR cast names its character set as a column's type does, ASCII and UNICODE included.
    ("Char OptFieldLen OptBinary", "Char OptFieldLen OptCharsetWithOptBinary"),
]

# Rules this grammar adds.
NEW = r"""
SelectIntoEarly:
	%prec lowerThanInto
|	"INTO" SelectIntoTarget

SelectIntoTarget:
	"OUTFILE" stringLit Fields Lines
|	"DUMPFILE" stringLit
|	SelectIntoVars

SelectIntoVars:
	SelectIntoVar
|	SelectIntoVars ',' SelectIntoVar

SelectIntoVar:
	singleAtIdentifier
|	Identifier

JsonValueOn:
	/* empty */
|	JsonTableOnResponse "ON" "EMPTY"
|	JsonTableOnResponse "ON" "ERROR"
|	JsonTableOnResponse "ON" "EMPTY" JsonTableOnResponse "ON" "ERROR"

SelectLockList:
	SelectLock
|	SelectLockList SelectLock

SelectLock:
	"FOR" "UPDATE" OfTablesOpt
|	"FOR" "SHARE" OfTablesOpt
|	"FOR" "UPDATE" OfTablesOpt "NOWAIT"
|	"FOR" "UPDATE" OfTablesOpt "WAIT" NUM
|	"FOR" "SHARE" OfTablesOpt "NOWAIT"
|	"FOR" "UPDATE" OfTablesOpt "SKIP" "LOCKED"
|	"FOR" "SHARE" OfTablesOpt "SKIP" "LOCKED"
|	"LOCK" "IN" "SHARE" "MODE"

SpatialType:
	"GEOMETRY"
|	"POINT"
|	"LINESTRING"
|	"POLYGON"
|	"MULTIPOINT"
|	"MULTILINESTRING"
|	"MULTIPOLYGON"
|	"GEOMETRYCOLLECTION"
|	"GEOMCOLLECTION"

RoutineType:
	Type
|	Type "COLLATE" CollationName

HistogramUpdate:
	/* empty */
|	"MANUAL" "UPDATE"
|	"AUTO" "UPDATE"
|	"USING" "DATA" stringLit

AlterViewStmt:
	"ALTER" ViewAlgorithm ViewDefiner ViewSQLSecurity "VIEW" ViewName ViewFieldList "AS" CreateViewSelectOpt ViewCheckOption

CheckTableStmt:
	"CHECK" TableOrTables TableNameList CheckTableOptions

CheckTableOptions:
	/* empty */
|	CheckTableOptions CheckTableOption

CheckTableOption:
	"FOR" "UPGRADE"
|	"QUICK"
|	"FAST"
|	"MEDIUM"
|	"EXTENDED"
|	"CHANGED"

TableRepairStmt:
	"REPAIR" NoWriteToBinLogAliasOpt TableOrTables TableNameList TableRepairOptions

TableRepairOptions:
	/* empty */
|	TableRepairOptions TableRepairOption

TableRepairOption:
	"QUICK"
|	"EXTENDED"
|	"USE_FRM"

ChecksumTableStmt:
	"CHECKSUM" TableOrTables TableNameList
|	"CHECKSUM" TableOrTables TableNameList "QUICK"
|	"CHECKSUM" TableOrTables TableNameList "EXTENDED"

LockInstanceStmt:
	"LOCK" "INSTANCE" "FOR" "BACKUP"

UnlockInstanceStmt:
	"UNLOCK" "INSTANCE"

DefaultRoleOpt:
	/* empty */
|	"DEFAULT" "ROLE" SetDefaultRoleOpt

BodyStartTransaction:
	"START" "TRANSACTION"
|	"START" "TRANSACTION" "READ" "WRITE"
|	"START" "TRANSACTION" "READ" "ONLY"
|	"START" "TRANSACTION" "WITH" "CONSISTENT" "SNAPSHOT"

JsonTable:
	"JSON_TABLE" '(' Expression ',' stringLit "COLUMNS" '(' JsonTableColumns ')' ')' TableAsNameOpt

JsonTableColumns:
	JsonTableColumn
|	JsonTableColumns ',' JsonTableColumn

JsonTableColumn:
	Identifier "FOR" "ORDINALITY"
|	Identifier RoutineType "PATH" stringLit JsonTableOn
|	Identifier RoutineType "EXISTS" "PATH" stringLit
|	"NESTED" "PATH" stringLit "COLUMNS" '(' JsonTableColumns ')'
|	"NESTED" stringLit "COLUMNS" '(' JsonTableColumns ')'

JsonTableOn:
	/* empty */
|	JsonTableOnResponse "ON" "EMPTY"
|	JsonTableOnResponse "ON" "ERROR"
|	JsonTableOnResponse "ON" "EMPTY" JsonTableOnResponse "ON" "ERROR"
|	JsonTableOnResponse "ON" "ERROR" JsonTableOnResponse "ON" "EMPTY"

JsonTableOnResponse:
	"NULL"
|	"ERROR"
|	"DEFAULT" stringLit

HandlerStmt:
	"HANDLER" TableName "OPEN"
|	"HANDLER" TableName "OPEN" Identifier
|	"HANDLER" TableName "OPEN" "AS" Identifier
|	"HANDLER" TableName "CLOSE"
|	"HANDLER" TableName "READ" HandlerRead WhereClauseOptional LimitClause

HandlerRead:
	"FIRST"
|	"NEXT"
|	Identifier HandlerCompare '(' ExpressionList ')'
|	Identifier "FIRST"
|	Identifier "NEXT"
|	Identifier "PREV"
|	Identifier "LAST"

HandlerCompare:
	eq
|	le
|	ge
|	'<'
|	'>'

XaStmt:
	"XA" "START" XaId XaStartOption
|	"XA" "BEGIN" XaId XaStartOption
|	"XA" "END" XaId
|	"XA" "END" XaId "SUSPEND"
|	"XA" "END" XaId "SUSPEND" "FOR" "MIGRATE"
|	"XA" "PREPARE" XaId
|	"XA" "COMMIT" XaId
|	"XA" "COMMIT" XaId "ONE" "PHASE"
|	"XA" "ROLLBACK" XaId
|	"XA" "RECOVER"
|	"XA" "RECOVER" "CONVERT" "XID"

XaStartOption:
	/* empty */
|	"JOIN"
|	"RESUME"

XaId:
	XaIdPart
|	XaIdPart ',' XaIdPart
|	XaIdPart ',' XaIdPart ',' NUM
|	XaIdPart ',' XaIdPart ',' hexLit

QualifyClause:
	/* empty */
|	"QUALIFY" Expression

IgnoreUnknownUser:
	/* empty */
|	"IGNORE" "UNKNOWN" "USER"

CacheIndexStmt:
	"CACHE" "INDEX" TableNameList "IN" Identifier

LoadIndexStmt:
	"LOAD" "INDEX" "INTO" "CACHE" TableNameList

XaIdPart:
	stringLit
|	hexLit
|	bitLit

ResetPersistStmt:
	"RESET" "PERSIST"
|	"RESET" "PERSIST" Identifier
|	"RESET" "PERSIST" "IF" "EXISTS" Identifier

ResetLogsStmt:
	"RESET" "BINARY" "LOGS" "AND" "GTIDS"
|	"RESET" "MASTER"
|	"RESET" "REPLICA"
|	"RESET" "REPLICA" "ALL"
|	"RESET" "SLAVE"
|	"RESET" "SLAVE" "ALL"

ComponentStmt:
	"INSTALL" "COMPONENT" ComponentList
|	"INSTALL" "COMPONENT" ComponentList "SET" VariableAssignmentList
|	"UNINSTALL" "COMPONENT" ComponentList
|	"INSTALL" "PLUGIN" Identifier "SONAME" stringLit
|	"UNINSTALL" "PLUGIN" Identifier

ComponentList:
	stringLit
|	ComponentList ',' stringLit

CreateTablespaceStmt:
	"CREATE" "TABLESPACE" Identifier TablespaceDatafile TablespaceOptions
|	"CREATE" "UNDO" "TABLESPACE" Identifier TablespaceDatafile TablespaceOptions

TablespaceDatafile:
	/* empty */
|	"ADD" "DATAFILE" stringLit

AlterTablespaceStmt:
	"ALTER" "TABLESPACE" Identifier TablespaceChange TablespaceOptions
|	"ALTER" "UNDO" "TABLESPACE" Identifier TablespaceChange TablespaceOptions

TablespaceChange:
	/* empty */
|	"ADD" "DATAFILE" stringLit
|	"DROP" "DATAFILE" stringLit
|	"RENAME" "TO" Identifier
|	"SET" "ACTIVE"
|	"SET" "INACTIVE"

DropTablespaceStmt:
	"DROP" "TABLESPACE" Identifier TablespaceOptions
|	"DROP" "UNDO" "TABLESPACE" Identifier TablespaceOptions

TablespaceOptions:
	/* empty */
|	TablespaceOptions TablespaceOption

TablespaceOption:
	Identifier EqOpt TablespaceValue

TablespaceValue:
	Identifier
|	stringLit
|	NUM

CreateFunctionStmt:
	"CREATE" OrReplace ViewAlgorithm ViewDefiner ViewSQLSecurity "FUNCTION" IfNotExists TableName '(' OptSpFuncParams ')' "RETURNS" RoutineType RoutineCharacteristics ProcedureProcStmt
|	"CREATE" OrReplace ViewAlgorithm ViewDefiner ViewSQLSecurity "FUNCTION" IfNotExists Identifier "RETURNS" LoadableFunctionType "SONAME" stringLit
|	"CREATE" "AGGREGATE" "FUNCTION" IfNotExists Identifier "RETURNS" LoadableFunctionType "SONAME" stringLit

LoadableFunctionType:
	"STRING"
|	"INTEGER"
|	"INT"
|	"REAL"
|	"DECIMAL"

OptSpFuncParams:
	/* empty */
|	SpFuncParams

SpFuncParams:
	SpFuncParam
|	SpFuncParams ',' SpFuncParam

SpFuncParam:
	Identifier RoutineType

RoutineCharacteristics:
	/* empty */
|	RoutineCharacteristics RoutineCharacteristic

RoutineCharacteristic:
	"COMMENT" stringLit
|	"LANGUAGE" "SQL"
|	"DETERMINISTIC"
|	"NOT" "DETERMINISTIC"
|	"CONTAINS" "SQL"
|	"NO" "SQL"
|	"READS" "SQL" "DATA"
|	"MODIFIES" "SQL" "DATA"
|	"SQL" "SECURITY" "DEFINER"
|	"SQL" "SECURITY" "INVOKER"

AlterRoutineStmt:
	"ALTER" "PROCEDURE" TableName AlterRoutineCharacteristics
|	"ALTER" "FUNCTION" TableName AlterRoutineCharacteristics

AlterRoutineCharacteristics:
	RoutineCharacteristic
|	AlterRoutineCharacteristics RoutineCharacteristic

CreateTriggerStmt:
	"CREATE" OrReplace ViewAlgorithm ViewDefiner ViewSQLSecurity "TRIGGER" IfNotExists TableName TriggerTime TriggerEvent "ON" TableName "FOR" "EACH" "ROW" TriggerOrder ProcedureProcStmt

TriggerTime:
	"BEFORE"
|	"AFTER"

TriggerEvent:
	"INSERT"
|	"UPDATE"
|	"DELETE"

TriggerOrder:
	/* empty */
|	"FOLLOWS" Identifier
|	"PRECEDES" Identifier

CreateEventStmt:
	"CREATE" OrReplace ViewAlgorithm ViewDefiner ViewSQLSecurity "EVENT" IfNotExists TableName "ON" "SCHEDULE" EventSchedule EventCompletion EventStatus EventComment "DO" ProcedureProcStmt

EventSchedule:
	"AT" Expression
|	"EVERY" Expression TimeUnit
|	"EVERY" Expression TimeUnit "STARTS" Expression
|	"EVERY" Expression TimeUnit "ENDS" Expression
|	"EVERY" Expression TimeUnit "STARTS" Expression "ENDS" Expression

EventCompletion:
	/* empty */
|	"ON" "COMPLETION" "PRESERVE"
|	"ON" "COMPLETION" "NOT" "PRESERVE"

EventStatus:
	/* empty */
|	"ENABLE"
|	"DISABLE"
|	"DISABLE" "ON" "REPLICA"
|	"DISABLE" "ON" "SLAVE"

EventComment:
	/* empty */
|	"COMMENT" stringLit

AlterEventStmt:
	"ALTER" ViewAlgorithm ViewDefiner ViewSQLSecurity "EVENT" TableName AlterEventOn AlterEventRename EventStatus EventComment AlterEventDo

AlterEventOn:
	/* empty */
|	"ON" "SCHEDULE" EventSchedule
|	"ON" "SCHEDULE" EventSchedule "ON" "COMPLETION" "PRESERVE"
|	"ON" "SCHEDULE" EventSchedule "ON" "COMPLETION" "NOT" "PRESERVE"
|	"ON" "COMPLETION" "PRESERVE"
|	"ON" "COMPLETION" "NOT" "PRESERVE"

AlterEventRename:
	/* empty */
|	"RENAME" "TO" TableName

AlterEventDo:
	/* empty */
|	"DO" ProcedureProcStmt

DropFunctionStmt:
	"DROP" "FUNCTION" IfExists TableName

DropTriggerStmt:
	"DROP" "TRIGGER" IfExists TableName

DropEventStmt:
	"DROP" "EVENT" IfExists TableName

SignalStmt:
	"SIGNAL" SignalCondition SignalInfo

ResignalStmt:
	"RESIGNAL" SignalInfo
|	"RESIGNAL" SignalCondition SignalInfo

SignalCondition:
	"SQLSTATE" optValue stringLit
|	Identifier

SignalInfo:
	/* empty */
|	"SET" SignalInfoItems

SignalInfoItems:
	SignalInfoItem
|	SignalInfoItems ',' SignalInfoItem

SignalInfoItem:
	Identifier eq Expression

GetDiagnosticsStmt:
	"GET" DiagnosticsArea "DIAGNOSTICS" DiagnosticsItems
|	"GET" DiagnosticsArea "DIAGNOSTICS" "CONDITION" Expression DiagnosticsItems

DiagnosticsArea:
	/* empty */
|	"CURRENT"
|	"STACKED"

DiagnosticsItems:
	DiagnosticsItem
|	DiagnosticsItems ',' DiagnosticsItem

DiagnosticsItem:
	DiagnosticsTarget eq Identifier

DiagnosticsTarget:
	Identifier
|	singleAtIdentifier
"""


def rule_span(grammar, name):
    m = re.search(r"(?ms)^" + re.escape(name) + r":.*?(?=^[A-Za-z_]\w*:|\Z)", grammar)
    if not m:
        raise SystemExit(f"no rule {name} to change")
    return m.start(), m.end()


def alternatives(grammar, name):
    start, end = rule_span(grammar, name)
    body = grammar[start:end].split(":", 1)[1]
    body = re.sub(r"\{\}", " ", body)
    return [" ".join(a.split()) for a in body.split("|")]


# Where a rule ends in an optional length or precision and a stored function's body may follow,
# `(` belongs to the length, as MySQL reads it: these empty alternatives give way to a shift of `(`.
GIVE_WAY_TO_PAREN = ["OptFieldLen", "FloatOpt", "OptBinary"]


def keyword_rules(grammar):
    """The tokens the rules that take a keyword as a name list."""
    out = set()
    for rule in ["UnReservedKeyword", "TiDBKeyword", "NotKeywordToken"]:
        out |= {a for a in alternatives(grammar, rule) if re.fullmatch(r'"[A-Z][A-Z0-9_]*"', a)}
    return out


def apply(grammar, unreserve=()):
    """The stripped grammar with the keywords and rules above, and with the tokens `unreserve` names
    taken as names too: TiDB's keywords that MySQL doesn't reserve."""
    head, rules = grammar.split("\n%%\n", 1)
    at = head.index("%precedence")
    head = head[:at] + "%token\n" + "".join(f'\t{token(w)} "{w}"\n' for w, _ in KEYWORDS) + "\n%precedence lowerThanParen\n%precedence lowerThanInto\n%precedence \"INTO\"\n" + head[at:]
    grammar = head + "\n%%\n" + rules
    extend = {name: list(alts) for name, alts in EXTEND.items()}
    replace = dict(REPLACE)
    extend.setdefault("UnReservedKeyword", []).extend([f'"{w}"' for w, reserved in KEYWORDS if not reserved] + list(unreserve))
    body_statements = [a for a in alternatives(grammar, "Statement") + extend["Statement"] if a not in NOT_IN_BODIES]
    replace["ProcedureStatementStmt"] = body_statements + ["BodyStartTransaction"]
    keywords = []
    for rule in ["UnReservedKeyword", "TiDBKeyword", "NotKeywordToken"]:
        keywords += [a for a in alternatives(grammar, rule) + extend.get(rule, []) if re.fullmatch(r'"[A-Z][A-Z0-9_]*"', a) and a[1:-1] not in NOT_LABELS]
    grammar = grammar.rstrip() + "\n\nLabelName:\n\tidentifier\n|\t" + "\n|\t".join(dict.fromkeys(keywords)) + "\n\n"
    for old, new in SUBSTITUTE:
        if old not in grammar:
            raise SystemExit(f"no text to replace: {old}")
        grammar = grammar.replace(old, new)
    for name, alts in replace.items():
        start, end = rule_span(grammar, name)
        grammar = grammar[:start] + name + ":\n\t" + "\n|\t".join(alts) + "\n\n" + grammar[end:]
    for name, alts in extend.items():
        start, end = rule_span(grammar, name)
        text = grammar[start:end].rstrip()
        grammar = grammar[:start] + text + "".join(f"\n|\t{a}" for a in alts) + "\n\n" + grammar[end:]
    for name in GIVE_WAY_TO_PAREN:
        start, end = rule_span(grammar, name)
        text = grammar[start:end]
        first = text.index(":") + 1
        # The empty alternative is the first, before its `|`.
        bar = text.index("|", first)
        if text[first:bar].replace("{}", "").strip():
            raise SystemExit(f"{name} doesn't open with its empty alternative")
        grammar = grammar[:start] + text[:first] + "\n\t%prec lowerThanParen\n" + text[bar:] + grammar[end:]
    return grammar.rstrip() + "\n" + NEW
