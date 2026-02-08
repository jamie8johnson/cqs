-- Stored procedure with params
CREATE PROCEDURE dbo.usp_GetOrders
    @CustomerID INT,
    @Status VARCHAR(50)
AS
BEGIN
    SELECT OrderID, OrderDate, Total
    FROM Orders
    WHERE CustomerID = @CustomerID AND Status = @Status
    ORDER BY OrderDate DESC;
END;
GO

-- Scalar function
CREATE FUNCTION dbo.fn_CalcTotal(@OrderID INT)
RETURNS DECIMAL(10,2)
AS
BEGIN
    DECLARE @Total DECIMAL(10,2)
    SELECT @Total = SUM(Quantity * UnitPrice)
    FROM OrderDetails WHERE OrderID = @OrderID;
    RETURN @Total
END;
GO

-- View
CREATE VIEW dbo.vw_ActiveCustomers
AS
SELECT CustomerID, Name, Email
FROM Customers
WHERE IsActive = 1;
GO

-- Trigger
CREATE TRIGGER trg_AuditInsert
ON Orders
AFTER INSERT
AS
BEGIN
    INSERT INTO AuditLog (TableName, Action, Timestamp)
    SELECT 'Orders', 'INSERT', GETDATE()
    FROM inserted;
END;
GO

-- Procedure that calls other procs/functions
CREATE PROCEDURE dbo.usp_ProcessOrder
    @OrderID INT
AS
BEGIN
    DECLARE @Total DECIMAL(10,2)
    SET @Total = dbo.fn_CalcTotal(@OrderID);
    EXEC dbo.usp_GetOrders @OrderID, 'Processed';
END;
GO
